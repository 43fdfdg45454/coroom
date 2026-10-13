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
       versiones antiguas) y los dispositivos que sobran o no tienen ranura.
    3. Crea un dispositivo raíz Root\coroomvhid por ranura (0 a -Pads - 1), cada uno con su ranura
       en el valor CoroomSlot de su clave de hardware.
    4. Añade el paquete al almacén de drivers y lo instala: pnputil /add-driver coroomvhid.inf /install.
    5. Limita la colección de control de cada mando a SYSTEM, administradores y el usuario con la
       sesión iniciada.

    No toca la firma de prueba, el arranque seguro ni la integridad de memoria (HVCI): el driver
    corre en modo usuario (UMDF2) bajo mshidumdf.sys y WUDFRd.sys, que son de Windows y están
    firmados por Microsoft.

    Hay que ejecutarlo en una consola de PowerShell abierta como administrador, desde la carpeta
    del paquete (la que tiene coroomvhid.inf) o con -PackageDir.

.PARAMETER Pads
    Mandos que se crean (1 a 8; 4 por defecto). Cada uno ocupa una ranura de invitado y los juegos
    lo ven siempre, aunque nadie lo use (en reposo). Para cambiar el número, vuelve a ejecutar
    install.ps1 con otro valor.

.EXAMPLE
    .\install.ps1
    .\install.ps1 -Pads 8
    .\install.ps1 -PackageDir .\out\x64
#>
[CmdletBinding()]
param(
    [string] $PackageDir,

    [ValidateRange(1, 8)]
    [int] $Pads = 4
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

$bySlot = @{}
foreach ($device in @(Get-CoroomDevice)) {
    $slot = Get-CoroomDeviceSlot -Device $device
    if ($null -eq $slot -or $slot -ge $Pads -or $bySlot.ContainsKey($slot)) {
        $why = if ($null -eq $slot) { 'sin ranura (versión anterior del driver)' } elseif ($slot -ge $Pads) { "ranura $($slot + 1) de más" } else { "ranura $($slot + 1) repetida" }
        Write-Host "Quitando el dispositivo $($device.InstanceId): $why"
        [CoroomVhid.SetupApi]::RemoveDevice($device.InstanceId)
    } else {
        $bySlot[$slot] = $device.InstanceId
    }
}

# 3. Un dispositivo raíz por ranura.
for ($slot = 0; $slot -lt $Pads; $slot++) {
    if ($bySlot.ContainsKey($slot)) {
        Write-Host "Ranura $($slot + 1): $($bySlot[$slot]) (ya existía)"
        continue
    }
    $id = [CoroomVhid.SetupApi]::CreateRootDevice($CoroomClassGuid, 'coroomvhid', $CoroomHardwareId,
        "Mando virtual de coroom $($slot + 1)", $CoroomSlotValue, [uint32]$slot)
    $bySlot[$slot] = $id
    Write-Host "Ranura $($slot + 1): $id (nuevo)"
}

# 4. Driver.
Write-Host 'Instalando el driver (pnputil)'
# 259 (ERROR_NO_MORE_ITEMS): el paquete entró en el almacén pero ningún dispositivo cambió de
# driver (ya tenía este u otro más nuevo).
$code = Invoke-PnpUtil -Arguments @('/add-driver', $inf, '/install') -AlsoAccept @(259)
if ($code -eq 259) {
    Write-Warning 'pnputil no actualizó ningún dispositivo: ¿ya estaba instalada esta versión del driver (o una más nueva)?'
}

# 5. Permisos de la colección de control (cuando hidclass ya la ha creado).
$failed = $false
foreach ($slot in ($bySlot.Keys | Sort-Object)) {
    $device = Get-PnpDevice -InstanceId $bySlot[$slot] -ErrorAction SilentlyContinue
    if (-not $device) {
        Write-Warning "No encuentro el dispositivo de la ranura $($slot + 1) ($($bySlot[$slot]))."
        $failed = $true
        continue
    }
    $controls = @()
    for ($try = 0; $try -lt 20 -and $controls.Count -eq 0; $try++) {
        $controls = @(Get-CoroomControlCollection -Device $device)
        if ($controls.Count -eq 0) {
            Start-Sleep -Milliseconds 500
        }
    }
    $status = (Get-PnpDevice -InstanceId $bySlot[$slot] -ErrorAction SilentlyContinue).Status
    Write-Host "Ranura $($slot + 1): $($bySlot[$slot]), estado $status"
    if ($status -ne 'OK') {
        $failed = $true
    }
    if ($controls.Count -eq 0) {
        Write-Warning "La ranura $($slot + 1) no tiene todavía su colección de control: no se han podido limitar sus permisos (vuelve a ejecutar install.ps1 cuando el dispositivo esté en marcha)."
        continue
    }
    foreach ($control in $controls) {
        try {
            [CoroomVhid.SetupApi]::SetSecurityAndRestart($control, $CoroomControlSddl)
        } catch {
            Write-Warning "No se pudieron limitar los permisos de $control ($($_.Exception.Message)); el mando funciona igual."
        }
    }
}

if ($failed) {
    Write-Warning 'Algún dispositivo no arrancó: mira su estado en el Administrador de dispositivos (Dispositivos de interfaz humana, «Mando virtual de coroom») y README.md, "Solución de problemas".'
}
if ($code -eq 3010) {
    Write-Warning 'Windows pide reiniciar para terminar la instalación.'
}
Write-Host "Listo: $Pads mandos. Comprueba con: coroom doctor (debe decir ""mandos: driver coroomvhid"")."
