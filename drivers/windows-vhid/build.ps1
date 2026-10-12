<#
.SYNOPSIS
    Compila el driver de mando virtual de coroom (coroomvhid.sys) y deja los paquetes en out\.

.DESCRIPTION
    Busca MSBuild (EWDK abierto con LaunchBuildEnv.cmd, o Visual Studio 2022 con el WDK) y el
    WDK, compila la solución para cada plataforma y copia cada paquete (coroomvhid.sys,
    coroomvhid.inf, coroomvhid.cat) a out\<plataforma>\ junto con los scripts de instalación.

    Con -TestSign firma el paquete con un certificado de prueba propio (lo crea la primera vez en
    Cert:\CurrentUser\My y lo exporta a out\coroomvhid-test.cer). Un paquete con firma de prueba
    solo carga en equipos con "bcdedit /set testsigning on"; para distribuirlo hace falta la firma
    por atestación de Microsoft (README.md).

.EXAMPLE
    .\build.ps1
    .\build.ps1 -Platform x64 -TestSign
#>
[CmdletBinding()]
param(
    [ValidateSet('x64', 'ARM64')]
    [string[]] $Platform = @('x64', 'ARM64'),

    [ValidateSet('Release', 'Debug')]
    [string] $Configuration = 'Release',

    [switch] $TestSign,

    [string] $CertificateSubject = 'CN=coroom test driver signing (NO DISTRIBUIR)',

    [string] $OutputDir = (Join-Path $PSScriptRoot 'out')
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Find-MSBuild {
    $inPath = Get-Command msbuild.exe -ErrorAction SilentlyContinue
    if ($inPath) {
        return $inPath.Source
    }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        $found = & $vswhere -latest -products * -requires Microsoft.Component.MSBuild `
            -find 'MSBuild\**\Bin\MSBuild.exe' | Select-Object -First 1
        if ($found) {
            return $found
        }
    }
    throw 'No encuentro MSBuild: instala Visual Studio 2022 (o sus Build Tools) con el WDK, o abre el EWDK (LaunchBuildEnv.cmd) y ejecuta este script desde ahí.'
}

function Find-KitsRoot {
    if ($env:WDKContentRoot -and (Test-Path $env:WDKContentRoot)) {
        return $env:WDKContentRoot.TrimEnd('\')
    }
    return (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10')
}

function Find-KitTool([string] $Name) {
    $inPath = Get-Command $Name -ErrorAction SilentlyContinue
    if ($inPath) {
        return $inPath.Source
    }
    $hostArch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
    $tool = Get-ChildItem (Join-Path (Find-KitsRoot) "bin\*\$hostArch\$Name") -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Parent.Name -match '^\d+(\.\d+)+$' } |
        Sort-Object { [version]$_.Directory.Parent.Name } -Descending |
        Select-Object -First 1
    if (-not $tool) {
        throw "No encuentro $Name en el WDK ($(Find-KitsRoot))."
    }
    return $tool.FullName
}

function Get-TestCertificate {
    $existing = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
        Where-Object { $_.Subject -eq $CertificateSubject -and $_.NotAfter -gt (Get-Date) } |
        Select-Object -First 1
    if ($existing) {
        return $existing
    }
    Write-Host "Creando el certificado de prueba «$CertificateSubject» en Cert:\CurrentUser\My"
    return New-SelfSignedCertificate -Type CodeSigningCert -Subject $CertificateSubject `
        -CertStoreLocation Cert:\CurrentUser\My -HashAlgorithm SHA256 -KeyUsage DigitalSignature `
        -NotAfter (Get-Date).AddYears(2)
}

$msbuild = Find-MSBuild
$kits = Find-KitsRoot
$vhf = Get-ChildItem (Join-Path $kits 'Include\*\km\vhf.h') -ErrorAction SilentlyContinue
if (-not $vhf) {
    throw "No encuentro el WDK (vhf.h) en $kits. Instálalo (la misma versión que el SDK de Windows) o usa el EWDK."
}
Write-Host "MSBuild: $msbuild"
Write-Host "WDK: $(($vhf | ForEach-Object { $_.Directory.Parent.Name }) -join ', ') en $kits"

$solution = Join-Path $PSScriptRoot 'coroomvhid.sln'
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

$certificate = $null
$signtool = $null
if ($TestSign) {
    $certificate = Get-TestCertificate
    $signtool = Find-KitTool 'signtool.exe'
    $cer = Join-Path $OutputDir 'coroomvhid-test.cer'
    Export-Certificate -Cert $certificate -FilePath $cer -Type CERT | Out-Null
    Write-Host "Certificado de prueba exportado a $cer"
}

foreach ($arch in $Platform) {
    Write-Host "== Compilando $Configuration|$arch"
    & $msbuild $solution /m /nologo /v:minimal "/p:Configuration=$Configuration" "/p:Platform=$arch"
    if ($LASTEXITCODE -ne 0) {
        throw "MSBuild falló para $arch (código $LASTEXITCODE)."
    }

    # El WDK deja el paquete (sys + inf con DriverVer + cat) en una carpeta con el nombre del
    # proyecto dentro de OutDir.
    $built = Join-Path $PSScriptRoot "build\$arch\$Configuration"
    $cat = Get-ChildItem $built -Recurse -Filter coroomvhid.cat | Select-Object -First 1
    if (-not $cat) {
        throw "No encuentro coroomvhid.cat en ${built}: ¿falló Inf2Cat?"
    }
    $package = Join-Path $OutputDir $arch
    Remove-Item $package -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $package | Out-Null
    foreach ($file in 'coroomvhid.sys', 'coroomvhid.inf', 'coroomvhid.cat') {
        Copy-Item (Join-Path $cat.DirectoryName $file) $package
    }
    Copy-Item (Join-Path $built 'coroomvhid.pdb') $package -ErrorAction SilentlyContinue

    if ($TestSign) {
        foreach ($file in 'coroomvhid.sys', 'coroomvhid.cat') {
            & $signtool sign /fd SHA256 /sha1 $certificate.Thumbprint /s My (Join-Path $package $file)
            if ($LASTEXITCODE -ne 0) {
                throw "signtool no pudo firmar $file (código $LASTEXITCODE)."
            }
        }
        Copy-Item (Join-Path $OutputDir 'coroomvhid-test.cer') $package
    }
    foreach ($script in 'install.ps1', 'uninstall.ps1', 'devices.ps1', 'README.md') {
        Copy-Item (Join-Path $PSScriptRoot $script) $package
    }
    Write-Host "Paquete $arch en $package"
}
