# RelayPlayer

Windows 上的 PS5 遥控游玩播放器。**没有采集卡也能用**：电脑和 PS5 通过网络连接，串流由开源项目 [chiaki-ng](https://github.com/chiaki-ng/chiaki-ng) 提供。

**[下载 Windows x64 版](https://github.com/MajoIreina/RelayPlayer/releases/download/v0.1.8/RelayPlayer-0.1.8-windows-x64.zip)** · [版本说明与 SHA-256](https://github.com/MajoIreina/RelayPlayer/releases/tag/v0.1.8)

## 第一次连 PS5

1. 将整个 ZIP 解压到自己的可写文件夹，例如 `D:\Apps\RelayPlayer`。打开 `RelayPlayer.exe`，点 **配对 / 连接 PS5**。首次会从 chiaki-ng 的 GitHub 发布页自动下载并校验约 **113 MiB** 的组件，然后打开连接窗口。无需单独安装后端、Rust 或 Python；之后在这个目录启动会复用组件。首次准备时请在解压盘预留约 500 MiB。
2. 让电脑和 PS5 连到同一局域网，在 PS5 上打开 **设定 → 系统 → 远程游玩 → 启动远程游玩**。在连接窗口选中自己的 PS5。若列表里没有主机，可在该窗口按 **F2** 手动输入 PS5 的局域网 IP；也可从 **Settings → Consoles → Register New** 打开注册表单。
3. 首次配对按连接窗口提示操作。已通过自己 PSN 登录且支持自动注册时，可选自动注册；如采用配对码，先选 PS5，再到主机的 **设定 → 系统 → 远程游玩 → 配对设备** 获取屏幕上的码。手动配对还需要该用户的 **PSN Account ID**；连接窗口可以协助获取，它不是公开昵称。配对成功后选中已注册的主机即可播放，下次不必重复输入配对码。

首次下载中断可在同一入口重试；若已安装的组件损坏，播放器会先验证新副本，再保留旧目录备份并切换。如果网络无法访问 GitHub，也可预先从[上游 v1.10.1](https://github.com/chiaki-ng/chiaki-ng/releases/tag/v1.10.1) 下载 `chiaki-ng-win_x64-MSYS2-portable-1.10.1.zip`，**不用解压**，执行 `RelayPlayer.exe --connect --backend-archive <压缩包路径>`。离线导入和在线下载使用相同的校验。

## 播放与设置

默认在 chiaki-ng 的独立窗口配对和播放，该窗口负责视频、音频、手柄、全屏和串流画质。请在播放窗口修改码率、分辨率等设置。局域网内再次使用已有配对时，RelayPlayer 不需要重新从 GitHub 下载组件。

另有[可选的 IPC 后端接口](docs/BACKEND.md)，用于提供嵌入式播放、采集卡或画质处理的后端。默认网络后端不提供宿主的 DLSS5 NR 开关、嵌入画面或 200 Mbps 锁定；公开版没有随附采集卡后端。

## 个人配对数据

发行包不包含账号、已配对主机、密钥或本地配置；它们由每位用户在首次使用时创建。网络后端的配对记录保存在当前 Windows 用户专属的 RelayPlayer 注册表配置中，同一用户升级可继续使用。它与单独安装的 Chiaki、旧 IPC 后端的配对数据分开；用 `RELAY_PLAYER_ROOT` 可再隔离一个测试配置。旧版配置无需删除。请勿分享自己已经运行过的播放器目录、注册表导出或 PSN 登录回调地址。

旧 IPC 用户仍可显式运行 `RelayPlayer.exe --ipc-backend`。需要本机旧授权时可运行 `RelayPlayer.exe --import-legacy`：仅补齐缺少的文件，设置优先沿用 0.1.5，授权依次查找 0.1.5、0.1.4、0.1.3；启用 `RELAY_PLAYER_ROOT` 的隔离实例禁止旧数据导入。旧授权不能代替 chiaki-ng 的首次配对。

## 从源码构建

需要 Windows x64、Rust MSVC 工具链、Visual Studio C++ 构建工具和 Windows SDK。在仓库根目录执行：

```powershell
cargo test --locked --release --target x86_64-pc-windows-msvc
cargo build --locked --release --target x86_64-pc-windows-msvc
```

生成 `target/x86_64-pc-windows-msvc/release/RelayPlayer.exe`。宿主使用 egui/eframe 和 windows-rs；见 [Cargo.lock](Cargo.lock)、[Rust 依赖与字体声明](notices/RUST-THIRD-PARTY.txt)、[Rust 标准库声明](notices/RUST-STANDARD-LIBRARY.html) 和 [chiaki-ng 来源与校验](notices/CHIAKI-NG.md)。

复核发行包时，先检出 `v0.1.8` 标签，再用 Git、Python 3.11+ 打包到空目录。为避免将开发机的个人路径嵌入发行程序，请映射构建路径；若自定义了 `CARGO_HOME`，也请对其做同类映射。

```powershell
$env:CARGO_ENCODED_RUSTFLAGS = @(
    "--remap-path-prefix=$env:USERPROFILE=/rust-home",
    "--remap-path-prefix=$($PWD.Path)=/relayplayer",
    '-C', 'link-arg=/PDBALTPATH:%_PDB%'
) -join [char]31
cargo build --locked --release --target x86_64-pc-windows-msvc
python tools/package_public.py --exe target/x86_64-pc-windows-msvc/release/RelayPlayer.exe --out-dir ../relay-release
```

打包工具只读取标签白名单中的 Git 文件和显式指定的 EXE，并逐个读回 ZIP 成员核对字节。[v0.1.7](docs/releases/v0.1.7.md) 保留为首个公开版本。
