# Build this plugin and stage it as a reader package: {Out}\mireader.pdf\{reader.toml, exe}.
#
#   .\stage.ps1                                    # debug -> ..\MimiReader\target\debug\readers
#   .\stage.ps1 -Release -Out D:\Programs\MimiReader\readers
param(
  [switch]$Release,
  # The release package's build (fat LTO, one codegen unit): the deploy's.
  [switch]$Dist,
  [string]$Out = ""
)
$ErrorActionPreference = "Stop"
$buildProfile = if ($Dist) { "reader-dist" } elseif ($Release) { "release" } else { "debug" }
if (-not $Out) { $Out = "$PSScriptRoot\..\MimiReader\target\$buildProfile\readers" }

$build = @("build", "--manifest-path", "$PSScriptRoot\Cargo.toml", "--bin", "mireader-reader-pdf")
if ($Dist) { $build += @("--profile", "reader-dist") } elseif ($Release) { $build += "--release" }
& cargo @build
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# The package folder is named after the manifest id; a running reader locks its exe.
$id = (Select-String -Path "$PSScriptRoot\reader.toml" -Pattern '^id\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
$package = Join-Path ([IO.Path]::GetFullPath($Out)) $id
New-Item -ItemType Directory -Force $package | Out-Null
Copy-Item "$PSScriptRoot\reader.toml" $package -Force
Copy-Item "$PSScriptRoot\target\$buildProfile\mireader-reader-pdf.exe" $package -Force
"staged $id -> $package"
