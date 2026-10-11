param()
$ErrorActionPreference='Stop'
$here=Split-Path -Parent $MyInvocation.MyCommand.Path
# 1. payload staging: syncthing.exe + cfg-A..D
$ps="$here\lspl"
if(Test-Path $ps){ Remove-Item $ps -Recurse -Force }
New-Item -ItemType Directory -Force -Path $ps | Out-Null
Expand-Archive -Path 'e:\Develop\duixiangoss\syncthing\syncthing-windows-amd64-v2.1.6.zip' -DestinationPath $ps
$win=Get-ChildItem $ps -Directory | Where-Object {$_.Name -like 'syncthing-windows*'} | Select-Object -First 1
Move-Item "$($win.FullName)\syncthing.exe" "$ps\syncthing.exe" -Force
Remove-Item $win.FullName -Recurse -Force
$cfgsrc='e:\Develop\duixiangoss\linksync\src\cfg'
foreach($L in 'A','B','C','D'){
  Copy-Item "$cfgsrc\$L" "$ps\cfg-$L" -Recurse
}
$payload="$here\lspayload.zip"
if(Test-Path $payload){ Remove-Item $payload -Force }
Compress-Archive -Path "$ps\syncthing.exe","$ps\cfg-A","$ps\cfg-B","$ps\cfg-C","$ps\cfg-D" -DestinationPath $payload -CompressionLevel Optimal
Remove-Item $ps -Recurse -Force
Write-Output ("PAYLOAD size=" + (Get-Item $payload).Length)
