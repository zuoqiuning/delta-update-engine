#requires -Version 7.0
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$root = $PSScriptRoot
$work = Join-Path $root 'work'
$keys = Join-Path $root 'keys'
$install = Join-Path $work 'install'
$v1 = Join-Path $work 'v1'
$v2 = Join-Path $work 'v2'
$public = Join-Path $work 'public'
$port = 0
$server = $null
$serveLog = Join-Path $env:TEMP "delta-serve-$PID.log"

function Step([string]$text) {
    Write-Host ""
    Write-Host "== $text ==" -ForegroundColor Cyan
}

# `serve --port 0` lets the OS pick a free port; read it back from the banner so
# the demo never collides with whatever else is listening on 8123.
function Wait-ForServePort([string]$log, $process) {
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline) {
        if ($process.HasExited) {
            throw "HTTP 服务启动失败（退出码 $($process.ExitCode)）"
        }
        if (Test-Path $log) {
            # Anchor on the banner ("serving <dir> at http://host:port") so a
            # second URL in the log cannot be mistaken for the listener.
            $match = [regex]::Match((Get-Content $log -Raw), 'at http://[^:\s]+:(\d+)')
            if ($match.Success) {
                return [int]$match.Groups[1].Value
            }
        }
        Start-Sleep -Milliseconds 200
    }
    throw "HTTP 服务 10 秒内没有打印监听端口"
}

try {
    Step '1/6 生成测试密钥'
    $keyPath = Join-Path $keys 'release-1.key'
    $pubPath = Join-Path $keys 'release-1.pub'
    if (!(Test-Path $keyPath) -or !(Test-Path $pubPath)) {
        cargo run -q -p delta-update-testkit --bin testkeygen -- $keyPath $pubPath
    }
    $publicKey = (Get-Content $pubPath -Raw).Trim()

    Step '2/6 编译演示程序'
    $demoManifest = Join-Path $root 'demo-app\Cargo.toml'
    cargo build -q --manifest-path $demoManifest
    $demoExe = (Resolve-Path (Join-Path $root 'demo-app\target\debug\demo-app.exe')).Path

    Step '3/6 启动本地 HTTP 服务（端口由系统分配）'
    if (Test-Path $work) { Remove-Item -Recurse -Force $work }
    New-Item -ItemType Directory -Force $install, $v1, $v2, $public | Out-Null
    $cli = (Resolve-Path (Join-Path $root '..\target\debug\delta-update.exe')).Path
    $server = Start-Process -FilePath $cli -ArgumentList 'serve', '--dir', "`"$public`"", '--port', '0' `
        -PassThru -WindowStyle Hidden -RedirectStandardOutput $serveLog
    $port = Wait-ForServePort $serveLog $server
    Write-Host "http://127.0.0.1:$port"

    Step '4/6 构造 v1 和 v2 两个软件目录'

    $config = @{
        endpoint           = "http://127.0.0.1:$port/manifests/latest.json"
        signing_public_key = $publicKey
    } | ConvertTo-Json

    foreach ($dir in @($v1, $v2)) {
        New-Item -ItemType Directory -Force (Join-Path $dir 'resources') | Out-Null
        Copy-Item $demoExe (Join-Path $dir 'demo-app.exe')
        Set-Content -Path (Join-Path $dir 'demo-config.json') -Value $config -NoNewline
    }
    Set-Content (Join-Path $v1 'version.txt') '1.0.0'
    Set-Content (Join-Path $v2 'version.txt') '2.0.0'

    # 2 MiB 资源只改 256 字节，用来观察“大文件小补丁”。
    $payload = New-Object byte[] 2097152
    [System.Random]::new(7).NextBytes($payload)
    [IO.File]::WriteAllBytes((Join-Path $v1 'resources\payload.bin'), $payload)
    $changed = [byte[]]$payload.Clone()
    for ($i = 0; $i -lt 256; $i++) {
        $changed[900000 + $i] = $changed[900000 + $i] -bxor 0x5A
    }
    [IO.File]::WriteAllBytes((Join-Path $v2 'resources\payload.bin'), $changed)

    Step '5/6 用真实 CLI 生成补丁、签名并发布到本地目录'
    & cargo run -q -p delta-update-cli -- release --from $v1 --from-version 1.0.0 --to $v2 --version 2.0.0 --bucket $public --key $keyPath --verbose

    Step '6/6 安装 v1，然后执行真实更新'
    Copy-Item -Path (Join-Path $v1 '*') -Destination $install -Recurse -Force
    & (Join-Path $install 'demo-app.exe') version
    & (Join-Path $install 'demo-app.exe') update

    # 等待 worker 真正写完 commit.status，而不是只看 version.txt：
    # version.txt 可能先落盘，此时 confirm 太早会留下 .old 备份。
    $statusFile = Join-Path $work 'state\journal\commit.status'
    $deadline = (Get-Date).AddSeconds(60)
    do {
        Start-Sleep -Milliseconds 250
        $current = (Get-Content (Join-Path $install 'version.txt') -Raw).Trim()
        $status = if (Test-Path $statusFile) { Get-Content $statusFile -Raw } else { '' }
    } while (($current -ne '2.0.0' -or $status -notmatch 'completed') -and (Get-Date) -lt $deadline)

    if ($current -ne '2.0.0' -or $status -notmatch 'completed') {
        throw "更新没有完成：version=$current status=$status"
    }

    Write-Host ""
    & (Join-Path $install 'demo-app.exe') version
    & (Join-Path $install 'demo-app.exe') confirm

    $backup = Join-Path $install '.old'
    $leftover = Get-ChildItem $backup -Directory -ErrorAction SilentlyContinue
    if ($leftover) {
        throw "confirm 之后仍有备份目录：$($leftover.FullName -join ', ')"
    }

    Write-Host ""
    Write-Host '发布产物：' -ForegroundColor Green
    Get-ChildItem $public -Recurse -File |
        Select-Object @{ Name = 'Size'; Expression = { $_.Length } }, FullName |
        Format-Table -AutoSize

    Write-Host '更新后的安装目录：' -ForegroundColor Green
    Get-ChildItem $install -Recurse -File |
        Select-Object @{ Name = 'Size'; Expression = { $_.Length } }, FullName |
        Format-Table -AutoSize

    $fullSize = (Get-Item (Join-Path $v2 'resources\payload.bin')).Length
    $patch = Get-ChildItem (Join-Path $public 'artifacts') -Recurse -File -Filter '*.tdp' |
        Sort-Object Length | Select-Object -First 1
    Write-Host ''
    if ($patch) {
        Write-Host "资源文件：$fullSize 字节，补丁：$($patch.Length) 字节"
    } else {
        Write-Host "资源文件：$fullSize 字节，未生成补丁（全部走了全量）"
    }
    Write-Host '真实更新完成：v1.0.0 -> v2.0.0' -ForegroundColor Green
    Write-Host "安装目录：$install"
    Write-Host "发布目录：$public"
}
finally {
    if ($server -and !$server.HasExited) {
        Stop-Process -Id $server.Id -Force
    }
    Remove-Item $serveLog -ErrorAction SilentlyContinue
}
