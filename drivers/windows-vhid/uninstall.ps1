<#
.SYNOPSIS
    Desinstala el driver de mando virtual de coroom.

.DESCRIPTION
    Quita los dispositivos Root\coroomvhid (con ellos desaparecen los mandos virtuales), borra el
    paquete del almacén de drivers (pnputil /delete-driver oemNN.inf /uninstall /force) y, con
    -RemoveTestCertificate, quita el certificado de prueba de los almacenes del equipo.
    La firma de prueba se desactiva aparte: bcdedit /set testsigning off y reiniciar.

    Hay que ejecutarlo en una consola de PowerShell abierta como administrador. Si coroom está
    abierto como anfitrión, ciérralo antes.

.EXAMPLE
    .\uninstall.ps1 -RemoveTestCertificate
#>
[CmdletBinding()]
param(
    [switch] $RemoveTestCertificate,

    [string] $CertificateSubject = 'CN=coroom test driver signing (NO DISTRIBUIR)'
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
    Write-Host "No hay dispositivos $CoroomHardwareId."
}

$packages = @(Get-CoroomDriverPackage)
foreach ($package in $packages) {
    Write-Host "Borrando el paquete $($package.Driver) del almacén de drivers"
    Invoke-PnpUtil -Arguments @('/delete-driver', $package.Driver, '/uninstall', '/force') | Out-Null
}
if ($packages.Count -eq 0) {
    Write-Host 'No hay paquetes coroomvhid.inf en el almacén de drivers.'
}

if ($RemoveTestCertificate) {
    foreach ($store in 'Cert:\LocalMachine\Root', 'Cert:\LocalMachine\TrustedPublisher') {
        Get-ChildItem $store | Where-Object { $_.Subject -eq $CertificateSubject } | ForEach-Object {
            Write-Host "Quitando el certificado de prueba de $store ($($_.Thumbprint))"
            Remove-Item $_.PSPath
        }
    }
}

Write-Host 'Listo. Si activaste la firma de prueba solo para coroom: bcdedit /set testsigning off y reinicia.'
