<#
.SYNOPSIS
    Compila el driver de mando virtual de coroom (coroomvhid.dll, UMDF2) y deja los paquetes en out\.

.DESCRIPTION
    Busca MSBuild (EWDK abierto con LaunchBuildEnv.cmd, o Visual Studio con el WDK) y el WDK, y por
    cada plataforma:
      1. compila la solución (la DLL y el INF con DriverVer y la versión de UMDF);
      2. comprueba el INF con InfVerif;
      3. con -Sign, firma coroomvhid.dll;
      4. crea el catálogo coroomvhid.cat con Inf2Cat;
      5. con -Sign, firma el catálogo;
      6. copia el paquete a out\<plataforma>\ con los scripts de instalación.

    Con -Sign crea al empezar un certificado autofirmado nuevo («CN=coroom (driver local)», solo
    para firmar código, SHA-256, clave no exportable) en Cert:\CurrentUser\My, firma con él (con
    sello de tiempo RFC 3161), deja en cada paquete solo su parte pública (coroomvhid.cer) y al
    terminar borra el certificado y su clave privada. Así nadie puede firmar después otra cosa con
    el certificado en el que confía quien instale el paquete. Es lo que hace la CI
    (.github/workflows/driver.yml) en cada compilación.

.PARAMETER Version
    Versión X.Y.Z de coroom: el DriverVer del INF será X.Y.Z.0. Sin ella, la pone stampinf.

.EXAMPLE
    .\build.ps1 -Sign
    .\build.ps1 -Platform x64 -Sign -Version 1.4.0
#>
[CmdletBinding()]
param(
    [ValidateSet('x64', 'ARM64')]
    [string[]] $Platform = @('x64', 'ARM64'),

    [ValidateSet('Release', 'Debug')]
    [string] $Configuration = 'Release',

    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string] $Version,

    [switch] $Sign,

    [string] $OutputDir = (Join-Path $PSScriptRoot 'out')
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$CertificateSubject = 'CN=coroom (driver local)'
$CodeSigningOid = '1.3.6.1.5.5.7.3.3'
# Servidores de sellado de tiempo RFC 3161 públicos; se prueba el siguiente si uno falla.
$TimestampServers = @(
    'http://timestamp.digicert.com',
    'http://timestamp.sectigo.com',
    'http://timestamp.acs.microsoft.com'
)

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
    throw 'No encuentro MSBuild: instala Visual Studio 2022 o 2026 (o sus Build Tools) con el WDK, o abre el EWDK (LaunchBuildEnv.cmd) y ejecuta este script desde ahí.'
}

function Find-KitsRoot {
    if ($env:WDKContentRoot -and (Test-Path $env:WDKContentRoot)) {
        return $env:WDKContentRoot.TrimEnd('\')
    }
    return (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10')
}

# Raíces donde buscar el WDK y el SDK: los paquetes NuGet de packages\ (c\ dentro de cada uno) y
# el WDK instalado o el EWDK.
function Get-KitRoots {
    $roots = @()
    $packages = Join-Path $PSScriptRoot 'packages'
    if (Test-Path $packages) {
        $roots += @(Get-ChildItem $packages -Directory | ForEach-Object { Join-Path $_.FullName 'c' } | Where-Object { Test-Path $_ })
    }
    $roots += Find-KitsRoot
    return $roots
}

# Una herramienta del WDK o del SDK (la de la versión más nueva): en el PATH, en bin\<versión>\ o
# tools\<versión>\, para la arquitectura del equipo o x86 (Inf2Cat solo está en x86).
function Find-KitTool([string] $Name) {
    $inPath = Get-Command $Name -ErrorAction SilentlyContinue
    if ($inPath) {
        return $inPath.Source
    }
    $hostArch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
    $tool = foreach ($root in Get-KitRoots) {
        foreach ($folder in 'bin', 'tools') {
            foreach ($arch in $hostArch, 'x86') {
                Get-ChildItem (Join-Path $root "$folder\*\$arch\$Name") -ErrorAction SilentlyContinue |
                    Where-Object { $_.Directory.Parent.Name -match '^\d+(\.\d+)+$' }
            }
        }
    }
    $tool = $tool | Sort-Object { [version]$_.Directory.Parent.Name } -Descending | Select-Object -First 1
    if (-not $tool) {
        throw "No encuentro $Name en el WDK ni en el SDK ($((Get-KitRoots) -join ', '))."
    }
    return $tool.FullName
}

# Certificado autofirmado de un solo uso: solo para firmar código y con la clave no exportable.
function New-EphemeralCertificate {
    $certificate = New-SelfSignedCertificate -Type CodeSigningCert -Subject $CertificateSubject `
        -FriendlyName 'coroom: firma del driver de mando (se borra al terminar la compilación)' `
        -CertStoreLocation Cert:\CurrentUser\My -KeyAlgorithm RSA -KeyLength 3072 `
        -HashAlgorithm SHA256 -KeyUsage DigitalSignature -KeyExportPolicy NonExportable `
        -NotAfter (Get-Date).AddYears(10)
    $eku = @($certificate.Extensions | Where-Object { $_ -is [Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension] })
    $usages = @($eku | ForEach-Object { $_.EnhancedKeyUsages } | ForEach-Object { $_.Value })
    if ($usages.Count -ne 1 -or $usages[0] -ne $CodeSigningOid) {
        Remove-EphemeralCertificate $certificate
        throw "El certificado nuevo tiene usos $($usages -join ', ') en vez de solo firma de código."
    }
    return $certificate
}

function Remove-EphemeralCertificate($Certificate) {
    $path = "Cert:\CurrentUser\My\$($Certificate.Thumbprint)"
    if (Test-Path $path) {
        Remove-Item $path -DeleteKey
    }
    if (Test-Path $path) {
        throw "No se pudo borrar el certificado $($Certificate.Thumbprint) de Cert:\CurrentUser\My."
    }
    Write-Host "Certificado $($Certificate.Thumbprint) y su clave privada borrados."
}

# Firma con el certificado de un solo uso y sello de tiempo RFC 3161 (SHA-256).
function Invoke-Sign([string] $SignTool, $Certificate, [string] $File) {
    foreach ($server in $TimestampServers) {
        & $SignTool sign /fd SHA256 /sha1 $Certificate.Thumbprint /s My /tr $server /td SHA256 /d 'coroom' $File
        if ($LASTEXITCODE -eq 0) {
            $signature = Get-AuthenticodeSignature -FilePath $File
            if (-not $signature.SignerCertificate -or $signature.SignerCertificate.Thumbprint -ne $Certificate.Thumbprint) {
                throw "$File no quedó firmado con el certificado $($Certificate.Thumbprint)."
            }
            if (-not $signature.TimeStamperCertificate) {
                throw "$File quedó firmado sin sello de tiempo."
            }
            return
        }
        Write-Warning "signtool no pudo firmar $File con el sello de $server (código $LASTEXITCODE); se prueba el siguiente."
    }
    throw "signtool no pudo firmar $File con ningún servidor de sellado de tiempo."
}

$msbuild = Find-MSBuild
$wdf = foreach ($root in Get-KitRoots) { Get-ChildItem (Join-Path $root 'Include\wdf\umdf\2.31\wdf.h') -ErrorAction SilentlyContinue }
if (-not $wdf) {
    throw "No encuentro el WDK (Include\wdf\umdf\2.31\wdf.h) en $((Get-KitRoots) -join ', '). Instálalo (la misma versión que el SDK de Windows), restaura packages.config o usa el EWDK."
}
Write-Host "MSBuild: $msbuild"
Write-Host "WDK: $(@($wdf)[0].FullName)"
$infverif = Find-KitTool 'infverif.exe'
$inf2cat = Find-KitTool 'Inf2Cat.exe'
$signtool = if ($Sign) { Find-KitTool 'signtool.exe' } else { $null }

$solution = Join-Path $PSScriptRoot 'coroomvhid.sln'
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$msbuildVersion = if ($Version) { "/p:CoroomDriverVersion=$Version.0" } else { '/p:CoroomDriverVersion=' }

$certificate = $null
try {
    if ($Sign) {
        $certificate = New-EphemeralCertificate
        Write-Host "Certificado de un solo uso: $($certificate.Subject), huella $($certificate.Thumbprint)"
    }

    foreach ($arch in $Platform) {
        Write-Host "== Compilando $Configuration|$arch"
        & $msbuild $solution /m /nologo /v:minimal "/p:Configuration=$Configuration" "/p:Platform=$arch" $msbuildVersion
        if ($LASTEXITCODE -ne 0) {
            throw "MSBuild falló para $arch (código $LASTEXITCODE)."
        }

        $built = Join-Path $PSScriptRoot "build\$arch\$Configuration"
        $dll = Join-Path $built 'coroomvhid.dll'
        if (-not (Test-Path $dll)) {
            throw "No encuentro $dll."
        }
        # El INF que deja stampinf (con DriverVer), en la carpeta de salida o en la intermedia.
        $stamped = Get-ChildItem (Join-Path $PSScriptRoot 'build') -Recurse -Filter coroomvhid.inf |
            Where-Object { $_.FullName -match "\\$arch\\$Configuration\\" -and (Select-String -Path $_.FullName -Pattern '^\s*DriverVer\s*=\s*\d' -Quiet) } |
            Sort-Object LastWriteTime -Descending | Select-Object -First 1
        if (-not $stamped) {
            throw "No encuentro el coroomvhid.inf con DriverVer de $arch en build\: ¿falló stampinf?"
        }

        $package = Join-Path $OutputDir $arch
        Remove-Item $package -Recurse -Force -ErrorAction SilentlyContinue
        New-Item -ItemType Directory -Force -Path $package | Out-Null
        Copy-Item $dll, $stamped.FullName $package
        Copy-Item (Join-Path $built 'coroomvhid.pdb') $package -ErrorAction SilentlyContinue
        $packageInf = Join-Path $package 'coroomvhid.inf'
        Write-Host "INF: $((Select-String -Path $packageInf -Pattern '^\s*DriverVer').Line.Trim())"

        # InfVerif: la validación básica tiene que pasar. La de "Windows Driver" (/w, aislamiento
        # del paquete) se muestra solo como aviso: la sección de Windows 10 añade los servicios
        # del sistema mshidumdf y WUDFRd a mano, como el ejemplo de Microsoft anterior a
        # Windows 11, porque MsHidUmdf.inf no existe allí.
        & $infverif /v $packageInf
        if ($LASTEXITCODE -ne 0) {
            throw "InfVerif encontró errores en coroomvhid.inf (código $LASTEXITCODE)."
        }
        & $infverif /w $packageInf | Out-Host
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "InfVerif /w (aislamiento) da avisos (código $LASTEXITCODE); ver el comentario de build.ps1."
        }

        if ($Sign) {
            Invoke-Sign $signtool $certificate (Join-Path $package 'coroomvhid.dll')
        }

        # Inf2Cat sobre una carpeta con solo el INF y la DLL (ya firmada).
        $staging = Join-Path $PSScriptRoot "build\cat\$arch"
        Remove-Item $staging -Recurse -Force -ErrorAction SilentlyContinue
        New-Item -ItemType Directory -Force -Path $staging | Out-Null
        Copy-Item $packageInf, (Join-Path $package 'coroomvhid.dll') $staging
        # En ARM64 Inf2Cat no acepta el genérico 10_ARM64: se nombran Windows 10 22H2 (VB) y 11.
        $os = if ($arch -eq 'ARM64') { '10_VB_ARM64,10_CO_ARM64,10_NI_ARM64' } else { '10_X64' }
        & $inf2cat "/driver:$staging" "/os:$os" /uselocaltime
        if ($LASTEXITCODE -ne 0) {
            throw "Inf2Cat falló para $arch (código $LASTEXITCODE)."
        }
        Copy-Item (Join-Path $staging 'coroomvhid.cat') $package

        if ($Sign) {
            Invoke-Sign $signtool $certificate (Join-Path $package 'coroomvhid.cat')
            # Solo la parte pública (DER): install.ps1 la instala en los almacenes del equipo.
            Export-Certificate -Cert $certificate -FilePath (Join-Path $package 'coroomvhid.cer') -Type CERT | Out-Null
        } else {
            Write-Warning "Paquete $arch sin firmar: Windows no lo instala. Usa -Sign o el paquete de la CI."
        }

        foreach ($file in 'install.ps1', 'uninstall.ps1', 'devices.ps1', 'README.md', 'coroom_vhid.h') {
            Copy-Item (Join-Path $PSScriptRoot $file) $package
        }
        Write-Host "Paquete $arch en $package"
        Get-ChildItem $package | Format-Table Name, Length | Out-Host
    }
} finally {
    if ($certificate) {
        Remove-EphemeralCertificate $certificate
    }
}
