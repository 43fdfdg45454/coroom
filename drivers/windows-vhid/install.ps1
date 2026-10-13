<#
.SYNOPSIS
    Instala el driver de mando virtual de coroom (coroomvhid.dll, UMDF2).

.DESCRIPTION
    1. Comprueba que coroomvhid.cer (el certificado autofirmado que trae el paquete) es el que firmó
       coroomvhid.cat y coroomvhid.dll y que solo sirve para firmar código, y lo instala en
       "Entidades de certificación raíz de confianza" y "Editores de confianza" del equipo. Su
       clave privada se borró al terminar la compilación que lo creó: nadie puede firmar nada más
       con él.
    2. Quita los paquetes coroomvhid.inf anteriores del almacén de drivers (también el KMDF de
       versiones antiguas) y los dispositivos de coroom que hubiera (los mandos raíz de las
       versiones anteriores incluidos).
    3. Crea el dispositivo de control Root\coroomvhid (los mandos se crean y se quitan desde él
       mientras coroom comparte) y cuatro Root\coroomxusb, mandos de Xbox 360 para XInput, cada uno
       con su ranura en el valor CoroomSlot de su clave de hardware. Los juegos solo ven los mandos
       que coroom está usando.
    4. Añade el paquete al almacén de drivers y lo instala: pnputil /add-driver coroomvhid.inf /install.
       Los permisos de cada dispositivo vienen en el INF.

    No toca la firma de prueba, el arranque seguro ni la integridad de memoria (HVCI): el driver
    corre en modo usuario (UMDF2) sobre WUDFRd.sys y vhf.sys, que son de Windows y están firmados
    por Microsoft.

    Hay que ejecutarlo en una consola de PowerShell abierta como administrador, desde la carpeta
    del paquete (la que tiene coroomvhid.inf) o con -PackageDir.

.EXAMPLE
    .\install.ps1
    .\install.ps1 -PackageDir .\out\x64
#>
[CmdletBinding()]
param(
    [string] $PackageDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'devices.ps1')

Assert-Administrator

if (-not $PackageDir) {
    $arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'ARM64' } else { 'x64' }
    $candidates = @($PSScriptRoot, (Join-Path $PSScriptRoot "out\$arch"))
    $PackageDir = $candidates | Where-Object { Test-Path (Join-Path $_ 'coroomvhid.cat') } | Select-Object -First 1
    if (-not $PackageDir) {
        throw "No encuentro el paquete (coroomvhid.inf + .dll + .cat + .cer): descárgalo de la release o compílalo con .\build.ps1 -Sign, o indica -PackageDir."
    }
}
$PackageDir = (Resolve-Path $PackageDir).Path
$inf = Join-Path $PackageDir 'coroomvhid.inf'
if (-not (Test-Path $inf)) {
    throw "No existe $inf."
}
Write-Host "Paquete: $PackageDir"

# 1. Certificado.
$certificate = Assert-CoroomPackageSignature -PackageDir $PackageDir
Write-Host "Certificado del paquete: $($certificate.Subject), huella $($certificate.Thumbprint), válido hasta $($certificate.NotAfter.ToString('yyyy-MM-dd'))"
foreach ($store in 'Cert:\LocalMachine\Root', 'Cert:\LocalMachine\TrustedPublisher') {
    if (Test-Path (Join-Path $store $certificate.Thumbprint)) {
        Write-Host "  ya estaba en $store"
    } else {
        Write-Host "  se instala en $store"
        Import-Certificate -FilePath (Join-Path $PackageDir 'coroomvhid.cer') -CertStoreLocation $store | Out-Null
    }
}

# 2. Paquetes anteriores y dispositivos que sobran.
foreach ($package in @(Get-CoroomDriverPackage)) {
    Write-Host "Quitando el paquete anterior $($package.Driver) ($($package.Version))"
    Invoke-PnpUtil -Arguments @('/delete-driver', $package.Driver, '/uninstall', '/force') | Out-Null
}

foreach ($device in @(Get-CoroomDevice)) {
    Write-Host "Quitando el dispositivo $($device.InstanceId)"
    [CoroomVhid.SetupApi]::RemoveDevice($device.InstanceId)
}

# 3. El dispositivo de control y los de Xbox 360.
$created = @()
$created += [CoroomVhid.SetupApi]::CreateRootDevice($CoroomClassGuid, 'coroomvhid', $CoroomHardwareId,
    'Mandos virtuales de coroom', $CoroomSlotValue, [uint32]0)
for ($slot = 0; $slot -lt $CoroomXusbSlots; $slot++) {
    $created += [CoroomVhid.SetupApi]::CreateRootDevice($CoroomClassGuid, 'coroomxusb', $CoroomXusbHardwareId,
        "Mando de Xbox 360 de coroom $($slot + 1)", $CoroomSlotValue, [uint32]$slot)
}
foreach ($id in $created) {
    Write-Host "Creado $id"
}

# 4. Driver.
Write-Host 'Instalando el driver (pnputil)'
# 259 (ERROR_NO_MORE_ITEMS): el paquete entró en el almacén pero ningún dispositivo cambió de
# driver (ya tenía este u otro más nuevo).
$code = Invoke-PnpUtil -Arguments @('/add-driver', $inf, '/install') -AlsoAccept @(259)
if ($code -eq 259) {
    Write-Warning 'pnputil no actualizó ningún dispositivo: ¿ya estaba instalada esta versión del driver (o una más nueva)?'
}

$failed = $false
foreach ($id in $created) {
    $status = $null
    for ($try = 0; $try -lt 20 -and $status -ne 'OK'; $try++) {
        $status = (Get-PnpDevice -InstanceId $id -ErrorAction SilentlyContinue).Status
        if ($status -ne 'OK') {
            Start-Sleep -Milliseconds 500
        }
    }
    Write-Host "$id, estado $status"
    if ($status -ne 'OK') {
        $failed = $true
    }
}

if ($failed) {
    Write-Warning 'Algún dispositivo no arrancó: mira su estado en el Administrador de dispositivos (Dispositivos del sistema, «Mandos virtuales de coroom» y «Mando de Xbox 360 de coroom») y README.md, "Solución de problemas".'
}
if ($code -eq 3010) {
    Write-Warning 'Windows pide reiniciar para terminar la instalación.'
}
Write-Host 'Listo. Comprueba con: coroom doctor (debe decir "mandos: driver coroomvhid").'
