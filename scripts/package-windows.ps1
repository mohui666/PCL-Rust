$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
# Portable releases must not require a separately installed VC runtime.
$env:RUSTFLAGS = (($env:RUSTFLAGS + ' -C target-feature=+crt-static').Trim())
cargo build --release --locked -p pcl-desktop -p pcl-cli
if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
$OutputFolder = Join-Path (Get-Location) 'dist\PCL-Rust-Windows'
New-Item -ItemType Directory -Force -Path $OutputFolder | Out-Null
Copy-Item -LiteralPath 'target\release\pcl-desktop.exe' -Destination (Join-Path $OutputFolder 'PCL-Rust.exe')
Copy-Item -LiteralPath 'target\release\pcl-cli.exe' -Destination (Join-Path $OutputFolder 'pcl-cli.exe')
Copy-Item -LiteralPath 'UPSTREAM-LICENCE','README.md' -Destination $OutputFolder
Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $OutputFolder 'PCL-Rust.exe')
