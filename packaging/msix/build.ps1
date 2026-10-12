<#
.SYNOPSIS
    Empaqueta coroom para Windows: un MSIX firmado y un .zip portátil.

.DESCRIPTION
    1. Compila `coroom` en release (salvo con -SkipBuild) con el FFmpeg LGPL de
       tools\ffmpeg.lock (tools\fetch-ffmpeg.ps1 lo deja en third_party\ffmpeg).
    2. Prepara en dist\ la carpeta con coroom.exe, las DLL de FFmpeg de third_party\ffmpeg\bin
       (enlazado dinámico: la LGPL lo pide así), sus licencias y, para el MSIX, Assets\ y
       AppxManifest.xml con la versión del workspace (Cargo.toml) como X.Y.Z.0.
    3. `makeappx pack` crea coroom-X.Y.Z-x64.msix y `signtool sign` lo firma:
       - con -TestCertificate, con un certificado de prueba CN=coroom-dev que crea la primera vez en
         Cert:\CurrentUser\My y exporta a dist\coroom-dev.cer (para instalarlo en los equipos de
         prueba; ver el README, «Windows»);
       - con -CertificatePath (y -CertificatePassword o la variable COROOM_SIGN_PASSWORD), con un
         .pfx real; -Publisher tiene que ser exactamente su sujeto;
       - sin ninguno de los dos, el MSIX queda sin firmar (no se puede instalar así).
    4. Comprime la carpeta (sin el manifiesto ni Assets) en coroom-X.Y.Z-windows-x64.zip.

    Necesita el SDK de Windows (makeappx.exe y signtool.exe); lo busca en el PATH o en
    "Windows Kits\10\bin\<versión>\x64".

.EXAMPLE
    .\packaging\msix\build.ps1 -TestCertificate
    .\packaging\msix\build.ps1 -SkipBuild -CertificatePath firma.pfx -Publisher 'CN=Mi editor, O=Mi editor, C=ES'
#>
[CmdletBinding()]
param(
    [switch] $SkipBuild,

    [switch] $TestCertificate,

    [string] $CertificatePath,

    [string] $CertificatePassword = $env:COROOM_SIGN_PASSWORD,

    [string] $Publisher = 'CN=coroom-dev',

    [string] $TimestampUrl = 'http://timestamp.digicert.com',

    [string] $OutputDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $OutputDir) {
    $OutputDir = Join-Path $root 'dist'
}

function Find-SdkTool([string] $Name) {
    $inPath = Get-Command $Name -ErrorAction SilentlyContinue
    if ($inPath) {
        return $inPath.Source
    }
    $programFiles = ${env:ProgramFiles(x86)}
    $tool = $null
    if ($programFiles) {
        $kits = Join-Path $programFiles 'Windows Kits\10\bin'
        $tool = Get-ChildItem (Join-Path $kits "*\x64\$Name") -ErrorAction SilentlyContinue |
            Where-Object { $_.Directory.Parent.Name -match '^\d+(\.\d+)+$' } |
            Sort-Object { [version]$_.Directory.Parent.Name } -Descending |
            Select-Object -First 1
    }
    if (-not $tool) {
        throw "No encuentro ${Name}: instala el SDK de Windows (viene con Visual Studio 2022 o sus Build Tools)."
    }
    return $tool.FullName
}

function Get-WorkspaceVersion {
    $manifest = Get-Content (Join-Path $root 'Cargo.toml') -Raw
    $match = [regex]::Match($manifest, '(?ms)^\[workspace\.package\].*?^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"')
    if (-not $match.Success) {
        throw 'No encuentro la versión en [workspace.package] de Cargo.toml.'
    }
    return '{0}.{1}.{2}' -f $match.Groups[1].Value, $match.Groups[2].Value, $match.Groups[3].Value
}

function Get-TestCertificate {
    $existing = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
        Where-Object { $_.Subject -eq $Publisher -and $_.NotAfter -gt (Get-Date) } |
        Select-Object -First 1
    if ($existing) {
        return $existing
    }
    Write-Host "Creando el certificado de prueba «$Publisher» en Cert:\CurrentUser\My"
    # Uso «firma de código» (1.3.6.1.5.5.7.3.3) y sin restricciones básicas de CA: lo que pide
    # Add-AppxPackage al certificado de un paquete.
    return New-SelfSignedCertificate -Type Custom -Subject $Publisher `
        -KeyUsage DigitalSignature -FriendlyName 'coroom (prueba, NO DISTRIBUIR)' `
        -CertStoreLocation Cert:\CurrentUser\My -HashAlgorithm SHA256 `
        -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}') `
        -NotAfter (Get-Date).AddYears(2)
}

$version = Get-WorkspaceVersion
Write-Host "coroom $version"

if (-not $SkipBuild) {
    if (-not (Test-Path (Join-Path $root 'third_party\ffmpeg\bin'))) {
        & (Join-Path $root 'tools\fetch-ffmpeg.ps1')
    }
    Push-Location $root
    try {
        cargo build --release --locked -p coroom
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build falló (código $LASTEXITCODE)."
        }
    }
    finally {
        Pop-Location
    }
}

$exe = Join-Path $root 'target\release\coroom.exe'
$ffmpeg = Join-Path $root 'third_party\ffmpeg'
if (-not (Test-Path $exe)) {
    throw "No encuentro ${exe}: compila antes (sin -SkipBuild)."
}
$dlls = Get-ChildItem (Join-Path $ffmpeg 'bin') -Filter '*.dll' -ErrorAction SilentlyContinue
if (-not $dlls) {
    throw "No encuentro las DLL de FFmpeg en $ffmpeg\bin: ejecuta tools\fetch-ffmpeg.ps1."
}

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$name = "coroom-$version-windows-x64"
$stage = Join-Path $OutputDir $name
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $stage | Out-Null
$licenses = New-Item -ItemType Directory -Force -Path (Join-Path $stage 'licencias')

Copy-Item $exe $stage
$dlls | Copy-Item -Destination $stage
Copy-Item (Join-Path $ffmpeg 'LICENSE.txt') (Join-Path $licenses 'FFmpeg-LGPL.txt')
Copy-Item (Join-Path $root 'tools\ffmpeg.lock') (Join-Path $licenses 'FFmpeg-origen.txt')
@"
coroom $version para Windows (x64)

Abre coroom.exe. Las DLL de esta carpeta son FFmpeg 8.1 en LGPL 2.1 o posterior, enlazado de forma
dinámica (puedes sustituirlas por otra build compatible); su licencia y su origen están en
licencias\. Más información en https://github.com/43fdfdg45454/coroom
"@ | Set-Content -Encoding utf8 (Join-Path $stage 'LEEME.txt')

# .zip portátil: sin manifiesto ni Assets.
$zip = Join-Path $OutputDir "$name.zip"
Remove-Item $zip -Force -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
Write-Host "Portátil: $zip"

# MSIX: la misma carpeta con el manifiesto (versión y editor) y los iconos.
$msixDir = Join-Path $OutputDir "$name-msix"
Remove-Item $msixDir -Recurse -Force -ErrorAction SilentlyContinue
Copy-Item $stage $msixDir -Recurse
Copy-Item (Join-Path $PSScriptRoot 'Assets') $msixDir -Recurse
[xml] $appx = Get-Content (Join-Path $PSScriptRoot 'AppxManifest.xml') -Raw
$appx.Package.Identity.Version = "$version.0"
$appx.Package.Identity.Publisher = $Publisher
$appx.Save((Join-Path $msixDir 'AppxManifest.xml'))

$makeappx = Find-SdkTool 'makeappx.exe'
$msix = Join-Path $OutputDir "coroom-$version-x64.msix"
& $makeappx pack /o /d $msixDir /p $msix
if ($LASTEXITCODE -ne 0) {
    throw "makeappx falló (código $LASTEXITCODE)."
}

$signtool = $null
if ($TestCertificate -or $CertificatePath) {
    $signtool = Find-SdkTool 'signtool.exe'
}
if ($TestCertificate) {
    $certificate = Get-TestCertificate
    $cer = Join-Path $OutputDir 'coroom-dev.cer'
    Export-Certificate -Cert $certificate -FilePath $cer -Type CERT | Out-Null
    & $signtool sign /fd SHA256 /sha1 $certificate.Thumbprint /s My $msix
    if ($LASTEXITCODE -ne 0) {
        throw "signtool no pudo firmar el MSIX (código $LASTEXITCODE)."
    }
    Write-Host "MSIX firmado con el certificado de prueba; su parte pública está en $cer"
}
elseif ($CertificatePath) {
    $arguments = @('sign', '/fd', 'SHA256', '/f', $CertificatePath, '/tr', $TimestampUrl, '/td', 'SHA256')
    if ($CertificatePassword) {
        $arguments += @('/p', $CertificatePassword)
    }
    & $signtool @arguments $msix
    if ($LASTEXITCODE -ne 0) {
        throw "signtool no pudo firmar el MSIX (código $LASTEXITCODE)."
    }
    Write-Host 'MSIX firmado con el certificado dado.'
}
else {
    Write-Warning 'MSIX sin firmar: Windows no lo instala así (usa -TestCertificate o -CertificatePath).'
}
Write-Host "MSIX: $msix"
