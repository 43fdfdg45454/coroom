<#
.SYNOPSIS
    Instala el driver de mando virtual de coroom (coroomvhid.sys).

.DESCRIPTION
    1. Si el paquete trae coroomvhid-test.cer (firma de prueba), lo instala en los almacenes
       "Entidades de certificación raíz de confianza" y "Editores de confianza" del equipo y
       comprueba que Windows arrancó con "bcdedit /set testsigning on".
    2. Crea el dispositivo raíz Root\coroomvhid si no existe.
    3. Añade el paquete al almacén de drivers y lo instala: pnputil /add-driver coroomvhid.inf /install.

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
        throw "No encuentro el paquete (coroomvhid.inf + .sys + .cat): compílalo con .\build.ps1 o indica -PackageDir."
    }
}
$inf = Join-Path (Resolve-Path $PackageDir) 'coroomvhid.inf'
if (-not (Test-Path $inf)) {
    throw "No existe $inf."
}
Write-Host "Paquete: $PackageDir"

$cer = Join-Path $PackageDir 'coroomvhid-test.cer'
if (Test-Path $cer) {
    if (-not (Test-TestSigning)) {
        throw @'
Este paquete tiene firma de prueba y Windows no arrancó con la firma de prueba activada.
Actívala y reinicia (en algunos equipos hay que desactivar antes el arranque seguro en la UEFI):
    bcdedit /set testsigning on
Después vuelve a ejecutar este script. Ver README.md, "Firma de prueba".
'@
    }
    Write-Host 'Instalando el certificado de prueba (raíz de confianza y editores de confianza del equipo)'
    Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
    Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null
}

$devices = @(Get-CoroomDevice)
if ($devices.Count -eq 0) {
    Write-Host "Creando el dispositivo raíz $CoroomHardwareId"
    [CoroomVhid.SetupApi]::CreateRootDevice($CoroomClassGuid, 'System', $CoroomHardwareId, 'coroom virtual gamepad bus')
} elseif ($devices.Count -gt 1) {
    Write-Warning "Hay $($devices.Count) dispositivos $CoroomHardwareId y el driver solo usa uno: ejecuta .\uninstall.ps1 y vuelve a instalar."
} else {
    Write-Host "El dispositivo raíz ya existe ($($devices[0].InstanceId)): se actualiza su driver"
}

Write-Host 'Instalando el driver (pnputil)'
# 259 (ERROR_NO_MORE_ITEMS): el paquete entró en el almacén pero ningún dispositivo cambió de
# driver (ya tenía este u otro más nuevo).
$code = Invoke-PnpUtil -Arguments @('/add-driver', $inf, '/install') -AlsoAccept @(259)
if ($code -eq 259) {
    Write-Warning 'pnputil no actualizó ningún dispositivo: ¿ya estaba instalada esta versión del driver (o una más nueva)?'
}

$device = Get-CoroomDevice | Select-Object -First 1
if ($device) {
    Write-Host "Dispositivo: $($device.InstanceId), estado: $($device.Status)"
    if ($device.Status -ne 'OK') {
        Write-Warning 'El dispositivo no arrancó: mira su estado en el Administrador de dispositivos (Dispositivos del sistema) y README.md, "Solución de problemas".'
    }
}
if ($code -eq 3010) {
    Write-Warning 'Windows pide reiniciar para terminar la instalación.'
}
Write-Host 'Listo. Comprueba con: coroom doctor (debe decir "mandos: driver coroomvhid").'
