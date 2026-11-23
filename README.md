# RelayPlayer

基于 egui/eframe 等开源项目构建的 Windows PS5 播放器宿主。串流、采集与画质处理需要另配兼容后端。

**v0.1.7 是首个公开版本。** 解压 Windows x64 宿主包，双击 `RelayPlayer.exe` 就能打开中文界面。包中没有播放后端或 GPU 运行库；仅凭此下载不能连接 PS5、显示采集卡画面或启用 DLSS。没有采集卡的用户可在接入支持网络串流的后端后使用网络模式。没有后端时，界面会显示接入提示。

## 播放界面

- 顶部的「连接」「采集」「画质」「手柄」「状态」分别管理对应功能。播放时可自动收起顶部栏；按 **F11** 全屏时上下栏和侧边设置面板均隐藏。按 **Esc** 或 **F11** 返回。
- 底部提供音量、静音和 NR 开关。画质页区分**已选设置**与**实际生效算法**：兼容后端须报告有效画面和运行状态，宿主才显示超分或 NR 生效。
- 目标码率滑条范围为 **2–200 Mbps**，每次调整 1 Mbps。「锁定」指请求保持本地目标，不能保证 PS5 的编码输出或网络实测码率达到目标值。

## 首次运行与个人数据

请解压到单独的可写文件夹。公开版不预置任何主机、账号或授权，默认也不读取其他版本的数据。当前实例的偏好写在自己的 `data/` 目录；发布 ZIP 不包含这个目录。不要用已配对的播放器文件夹重新打包。

在**自己的电脑**上需要沿用旧版授权时，将新目录与旧版并排，使用原 Windows 用户，在新版目录运行：

```powershell
.\RelayPlayer.exe --import-legacy
```

只有显式指定此参数才尝试导入。设置仅从 0.1.5 补齐；加密授权按 0.1.5、0.1.4、0.1.3 的顺序查找。只复制缺少的文件，不覆盖当前实例，不修改旧版；指定 `RELAY_PLAYER_ROOT` 的隔离实例始终跳过导入。导入并不会为宿主安装播放后端。

## 构建与接入

在 Windows x64 上使用 Rust MSVC 工具链、Visual Studio C++ 构建工具和 Windows SDK，从本仓库根目录执行：

```powershell
cargo test --locked --release --target x86_64-pc-windows-msvc
cargo build --locked --release --target x86_64-pc-windows-msvc
```

生成文件为 `target/x86_64-pc-windows-msvc/release/RelayPlayer.exe`。构建和宿主测试不需要 PS5、Sony 登录或采集卡。如何编写兼容后端、传递状态和提供可嵌入的视频窗口，见 [播放后端接口](docs/BACKEND.md)。

## 来源与发布文件

界面使用 egui/eframe，Windows 接口使用 windows-rs，后端消息通过 serde_json 处理。在公开源码包中，具体版本、上游项目和各自的许可证见 [Cargo.lock](Cargo.lock)、[依赖与字体声明](notices/RUST-THIRD-PARTY.txt) 和 [Rust 标准库声明](notices/RUST-STANDARD-LIBRARY.html)。源码中的 [几何绘图脚本](tools/make_icon.py) 可复现应用图标。

[v0.1.7 发布说明](docs/releases/v0.1.7.md) 列出了宿主、公开源码和 SHA-256 校验文件。源码包仅从版本标签的允许文件清单生成；宿主包仅包含可执行文件和指定文档。

如需本地复核发行包，先检出与 `HEAD` 对应的 `v0.1.7` 标签，在空的构建目录重编宿主。用于分发的 EXE 不能包含本机的个人目录路径。Windows PowerShell 示例（若自定义了 `CARGO_HOME`，也请对该路径做同类映射）：

```powershell
$env:CARGO_ENCODED_RUSTFLAGS = @(
    "--remap-path-prefix=$env:USERPROFILE=/rust-home",
    "--remap-path-prefix=$($PWD.Path)=/relayplayer",
    '-C', 'link-arg=/PDBALTPATH:%_PDB%'
) -join [char]31
cargo build --locked --release --target x86_64-pc-windows-msvc
python tools/package_public.py --exe target/x86_64-pc-windows-msvc/release/RelayPlayer.exe --out-dir ../relay-public-artifacts
```

打包工具需要 Git 和 Python 3.11+，输出目录须为空或尚不存在。它只从标签读取白名单中的源码和文档，并逐个读回 ZIP 成员核对字节。
