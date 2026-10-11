$ErrorActionPreference='Stop'
$here='e:\Develop\duixiangoss\linksync-tauri\payload'
$cfgsrc='e:\Develop\duixiangoss\linksync\src\cfg'
$tmp="$here\stage"

function Make-Payload([string]$bin, [string]$binName, [string]$out) {
  if(Test-Path $tmp){ Remove-Item $tmp -Recurse -Force }
  New-Item -ItemType Directory -Force -Path $tmp | Out-Null
  Copy-Item $bin "$tmp\$binName"
  foreach($L in 'A','B','C','D'){ Copy-Item "$cfgsrc\$L" "$tmp\cfg-$L" -Recurse }
  if(Test-Path $out){ Remove-Item $out -Force }
  Compress-Archive -Path "$tmp\$binName","$tmp\cfg-A","$tmp\cfg-B","$tmp\cfg-C","$tmp\cfg-D" -DestinationPath $out -CompressionLevel Optimal
  Remove-Item $tmp -Recurse -Force
  Write-Output ("PAYLOAD $out size=" + (Get-Item $out).Length)
}

# Linux: extract from tar.gz
$lxdir="$here\lx"
New-Item -ItemType Directory -Force -Path $lxdir | Out-Null
tar -xzf 'e:\Develop\duixiangoss\syncthing\syncthing-linux-amd64.tar.gz' -C $lxdir
$lxb = Get-ChildItem $lxdir -Recurse -Filter 'syncthing' | Where-Object { -not $_.PSIsContainer } | Sort-Object Length -Descending | Select-Object -First 1
Make-Payload $lxb.FullName 'syncthing' "$here\lspayload-linux.zip"
Remove-Item $lxdir -Recurse -Force

# macOS: extract from zip
$mcdir="$here\mc"
New-Item -ItemType Directory -Force -Path $mcdir | Out-Null
Expand-Archive -Path 'e:\Develop\duixiangoss\syncthing\syncthing-macos-universal.zip' -DestinationPath $mcdir
$mcb = Get-ChildItem $mcdir -Recurse -Filter 'syncthing' | Where-Object { -not $_.PSIsContainer } | Sort-Object Length -Descending | Select-Object -First 1
Make-Payload $mcb.FullName 'syncthing' "$here\lspayload-mac.zip"
Remove-Item $mcdir -Recurse -Force
