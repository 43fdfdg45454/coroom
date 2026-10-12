# Descarga la build de FFmpeg fijada en tools/ffmpeg.lock (BtbN, LGPL compartida, win64) y la deja
# en third_party\ffmpeg\, que es donde la busca la compilación (.cargo\config.toml).
#
#   tools\fetch-ffmpeg.ps1            descarga y comprueba la build de Windows del lock
#   tools\fetch-ffmpeg.ps1 -Force     la vuelve a descargar aunque ya esté
#   tools\fetch-ffmpeg.ps1 -Dest DIR  la extrae en DIR en lugar de third_party\ffmpeg
#
# Para fijar un release nuevo en el lock: tools/fetch-ffmpeg.sh --pin (desde Linux o WSL).
# Las DLL quedan en third_party\ffmpeg\bin: coroom-codec las copia junto a los ejecutables de
# target\<perfil>\ al compilar; para los tests, añade ese directorio al PATH (la CI lo hace).

[CmdletBinding()]
param(
    [switch]$Force,
    [string]$Dest
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Root = Split-Path -Parent $PSScriptRoot
$Lock = Join-Path $Root 'tools/ffmpeg.lock'
if (-not $Dest) { $Dest = Join-Path $Root 'third_party/ffmpeg' }
$Repo = 'BtbN/FFmpeg-Builds'

# Lee CLAVE=valor del lock sin ejecutarlo.
$Values = @{}
foreach ($line in Get-Content -LiteralPath $Lock -Encoding UTF8) {
    if ($line -match '^\s*([A-Z0-9_]+)=(.*)$') {
        $Values[$Matches[1]] = $Matches[2].Trim()
    }
}
$Version = $Values['FFMPEG_VERSION']
$Tag = $Values['BTBN_TAG']
$File = $Values['WINDOWS_FILE']
$Sha = $Values['WINDOWS_SHA256']
if (-not $Tag -or -not $File -or -not $Sha) {
    Write-Error ("tools/ffmpeg.lock no tiene fijada la build de Windows (etiqueta, fichero y " +
        "SHA-256). Fíjala con 'tools/fetch-ffmpeg.sh --pin' desde una máquina con acceso a " +
        "github.com, o usa otra build de FFmpeg >= 8.0 con FFMPEG_INCLUDE_DIR y FFMPEG_LIBS_DIR " +
        "(ver README.md, «FFmpeg»).")
}

$Stamp = Join-Path $Dest '.coroom-ffmpeg'
$Want = "$Tag $File $Sha"
if (-not $Force -and (Test-Path -LiteralPath $Stamp) -and
    ((Get-Content -LiteralPath $Stamp -Raw).Trim() -eq $Want)) {
    Write-Host "FFmpeg $Version ($Tag) ya está en $Dest."
} else {
    $Work = Join-Path ([System.IO.Path]::GetTempPath()) ("coroom-ffmpeg-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $Work | Out-Null
    try {
        $Archive = Join-Path $Work $File
        $Url = "https://github.com/$Repo/releases/download/$Tag/$File"
        Write-Host "Descargando $File ($Tag)…"
        Invoke-WebRequest -Uri $Url -OutFile $Archive -UseBasicParsing
        $Actual = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($Actual -ne $Sha.ToLowerInvariant()) {
            Write-Error "el SHA-256 de $File ($Actual) no coincide con tools/ffmpeg.lock ($Sha)"
        }
        $Extracted = Join-Path $Work 'x'
        Expand-Archive -LiteralPath $Archive -DestinationPath $Extracted
        # El zip trae un único directorio raíz (ffmpeg-…-win64-lgpl-shared-X.Y).
        $Top = Get-ChildItem -LiteralPath $Extracted -Directory | Select-Object -First 1
        if (-not $Top -or -not (Test-Path -LiteralPath (Join-Path $Top.FullName 'include/libavcodec/avcodec.h'))) {
            Write-Error "$File no trae include\libavcodec"
        }
        if (Test-Path -LiteralPath $Dest) { Remove-Item -LiteralPath $Dest -Recurse -Force }
        New-Item -ItemType Directory -Path (Split-Path -Parent $Dest) -Force | Out-Null
        Move-Item -LiteralPath $Top.FullName -Destination $Dest
        Set-Content -LiteralPath $Stamp -Value $Want -Encoding UTF8 -NoNewline
        Write-Host "FFmpeg $Version extraído en $Dest."
    } finally {
        Remove-Item -LiteralPath $Work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

$Exe = Join-Path $Dest 'bin/ffmpeg.exe'
if (Test-Path -LiteralPath $Exe) {
    & $Exe -hide_banner -version | Select-Object -First 1
    Write-Host 'Codificadores de interés:'
    & $Exe -hide_banner -encoders | Select-String -Pattern 'vulkan|nvenc|amf|qsv|vaapi|svtav1'
    Write-Host 'Decodificadores de interés:'
    & $Exe -hide_banner -decoders | Select-String -Pattern 'dav1d|cuvid|qsv'
}
