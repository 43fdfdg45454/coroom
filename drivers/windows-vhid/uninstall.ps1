<#
.SYNOPSIS
    Desinstala el driver de mando virtual de coroom.

.DESCRIPTION
    Quita los dispositivos Root\coroomvhid y Root\coroomxusb (con ellos desaparecen los mandos
    virtuales), borra el
    paquete del almacén de drivers (pnputil /delete-driver oemNN.inf /uninstall /force) y quita de
    "Entidades de certificación raíz de confianza" y "Editores de confianza" del equipo los
    certificados de los paquetes de coroom: los de sujeto "CN=coroom (driver local)" (y el de prueba
    de las versiones KMDF) y, si está en esta carpeta, el de huella igual a coroomvhid.cer.

    No hay nada más que deshacer: la instalación no toca la firma de prueba, el arranque seguro ni
    la integridad de memoria.

    Hay que ejecutarlo en una consola de PowerShell abierta como administrador. Si coroom está
    abierto como anfitrión, ciérralo antes.

.PARAMETER KeepCertificate
    No quita los certificados (por ejemplo, para reinstalar enseguida el mismo paquete).

.EXAMPLE
    .\uninstall.ps1
#>
[CmdletBinding()]
param(
    [switch] $KeepCertificate
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'devices.ps1')

Assert-Administrator

$devices = @(Get-CoroomDevice)
foreach ($device in $devices) {
    Write-Host "Quitando el dispositivo $($device.InstanceId)"
    [CoroomVhid.SetupApi]::RemoveDevice($device.InstanceId)
}
if ($devices.Count -eq 0) {
    Write-Host "No hay dispositivos $CoroomHardwareId ni $CoroomXusbHardwareId."
}

$packages = @(Get-CoroomDriverPackage)
foreach ($package in $packages) {
    Write-Host "Borrando el paquete $($package.Driver) del almacén de drivers"
    Invoke-PnpUtil -Arguments @('/delete-driver', $package.Driver, '/uninstall', '/force') | Out-Null
}
if ($packages.Count -eq 0) {
    Write-Host 'No hay paquetes coroomvhid.inf en el almacén de drivers.'
}

if ($KeepCertificate) {
    Write-Host 'Los certificados se quedan (-KeepCertificate).'
} else {
    $thumbprints = @()
    $cer = Join-Path $PSScriptRoot 'coroomvhid.cer'
    if (Test-Path $cer) {
        $thumbprints += [Security.Cryptography.X509Certificates.X509Certificate2]::new($cer).Thumbprint
    }
    $removed = 0
    foreach ($store in 'Cert:\LocalMachine\Root', 'Cert:\LocalMachine\TrustedPublisher') {
        foreach ($certificate in @(Get-CoroomCertificate -Store $store -Thumbprint $thumbprints)) {
            Write-Host "Quitando el certificado $($certificate.Subject) ($($certificate.Thumbprint)) de $store"
            Remove-Item $certificate.PSPath
            $removed++
        }
    }
    if ($removed -eq 0) {
        Write-Host 'No había certificados de coroom en los almacenes del equipo.'
    }
}

Write-Host 'Listo.'
