LinkSync 1.0.0 正式版，基于 Tauri 2 与 Rust 的跨平台文件同步客户端。

## 功能

- 文件自动同步：局域网直连优先，跨网段自动走中继，支持 90 天历史版本恢复
- 成员账号登录、自助修改密码、管理员面板与登录记录
- 手机网页上传、文件分享链接、同步限速设置
- 屏幕共享：WebRTC 直连优先，失败自动降级 TCP 中继，再降级快照模式
- 系统托盘常驻、开机自启、单实例运行

## 下载

| 平台 | 文件 |
| --- | --- |
| Windows x64 | LinkSync-1.0.0-windows-x64.exe |
| Windows ARM64 | LinkSync-1.0.0-windows-arm64.exe |
| Linux ARM64 | LinkSync-1.0.0-linux-arm64.deb |

Windows 版本为单文件绿色程序，双击即可运行；ARM64 版本适用于骁龙等 ARM 平台设备。Linux 版本通过 dpkg 安装，需系统具备 WebKitGTK 4.1。
