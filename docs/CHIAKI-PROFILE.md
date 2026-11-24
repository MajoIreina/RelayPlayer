# Windows 版 chiaki-ng 的独立配置

上游 v1.10.1 的 Windows 便携版使用 Qt 的 `QSettings`。传入 `--profile` 仍会初始化默认设置和共享渲染设置，并可能更改当前 profile；改变 `APPDATA` 或工作目录也不能隔离当前用户注册表。RelayPlayer 为自己的串流进程使用独立的 Qt **组织名和应用名**。首次运行从空配置配对，不会沿用同一用户独立安装的 Chiaki。

## 公开源码与实际改动

使用 [上游 v1.10.1](https://github.com/chiaki-ng/chiaki-ng/releases/tag/v1.10.1) 的 Windows x64 便携包；固定提交为 `f30da0aaeb96fc0026cc03683153f682352e7d7c`。相关来源：[初始化组织名/应用名](https://github.com/chiaki-ng/chiaki-ng/blob/f30da0aaeb96fc0026cc03683153f682352e7d7c/gui/src/main.cpp)、[Qt 设置对象](https://github.com/chiaki-ng/chiaki-ng/blob/f30da0aaeb96fc0026cc03683153f682352e7d7c/gui/src/settings.cpp)、[QML 单例](https://github.com/chiaki-ng/chiaki-ng/blob/f30da0aaeb96fc0026cc03683153f682352e7d7c/gui/src/qmlbackend.cpp)、[上游许可证](https://github.com/chiaki-ng/chiaki-ng/blob/f30da0aaeb96fc0026cc03683153f682352e7d7c/LICENSES/AGPL-3.0-only-OpenSSL.txt)。

在 C++ 源码层面，修改等价于把 `gui/src/main.cpp` 中 `QGuiApplication::setOrganizationName(...)` 和 `QGuiApplication::setApplicationName(...)` 的参数由 `Chiaki` 同时改为选定的六个 ASCII 字符。显示名称仍为 chiaki-ng。上游 QML 单例也叫 `Chiaki`，必须保持原样。编译整个上游项目时只需在这两处使用同一个专用名称；按上游的递归子模块和 Windows 构建流程生成运行库。

RelayPlayer 的实现为接收方本地生成这一副本。原始 ZIP 的 SHA-256 为 `6ca3f5e6780608104d79345ab7755cfa77fefc33a41ae10651a874c43f54ea57`。其中 `chiaki.exe`（6,543,375 字节、AMD64、没有 PE Authenticode 签名）的 SHA-256 为 `c69a1e2dbf7406f74ae8a33c7c98fba226d79431a36aca5bc7143cedc4d19dec`。下列地址只适用于这个文件：

| 调用 | 读名称的指令 RVA | `call` RVA | 所指的 Qt6Core 导入槽 RVA |
| --- | --- | --- | --- |
| `QCoreApplication::setOrganizationName(QString const&)` | `0x1217c` | `0x121ab` | `0x4da6a0` |
| `QCoreApplication::setApplicationName(QString const&)` | `0x121c7` | `0x121ef` | `0x4da690` |

两个 RIP 相对 `lea` 都指向只读段的 RVA `0x223dbb`（文件偏移 `0x221dbb`），构造参数时都使用长度 6，并经 `QString::fromUtf8` 转换。偏移 `0x22af6d` 的另一个 `Chiaki` 由 QML 单例使用。初始化脚本固定完整原文件摘要，并复核指令目标、导入槽和相邻字节，只替换前一处的六个字符，连结尾 NUL 和 PE 头都不改。串流协议、账户授权、QML、亲自配对的要求以及 DLL 均无改动。v0.1.9 额外对本机生成副本应用下述姿态修正。上游更新时必须重新审核；不得移除摘要检查后直接使用此方法。

## 文件与注册表

- `tools/ensure-chiaki.ps1` 先验证完整上游目录。`tools/Initialize-ChiakiProfile.ps1 -InstallRoot <播放器的 backends 目录>` 再从其中的 `chiaki-ng/chiaki.exe` 生成固定的 `chiaki-ng/relay-profile.exe`。从不执行原始模板，不在个人配置目录存放 EXE、DLL 或插件。目录里最多保留一个配置专用副本，切换时在同一卷上原子更换。临时文件放在指定的 `backends` 下并清理。
- 常规运行使用稳定的默认标识，因此同一 Windows 用户升级 RelayPlayer 后可继续使用此前在**公开后端**中配对的主机。可选 `-ProfileRoot <本地绝对路径>`（由宿主从显式的 `RELAY_PLAYER_ROOT` 传入）仅标识另一套配置，不能选择要执行的代码。路径统一分隔符和大小写；不同路径可用于无个人数据的测试。Qt 的授权仍按 Windows 用户保存在注册表，不是数据目录中的可携带文件。
- 六字符值由域分隔的 SHA-256 派生，格式为 `R` 加五个大写 Base32 字符。`HKCU\Software\<六字符>` 上单独记录完整 SHA-256 绑定；默认设置在其同名子键，渲染设置在 `pl_render_params` 子键。检测到短名称碰撞、既有无标记的应用配置或同名系统注册表设置时拒绝运行。本工具不会枚举普通 Chiaki 的键，也不会读取账号、配对码或串流凭证。
- 启动约定：成功时最后一行仅为 `RELAY_PROFILE <六字符> <派生文件的 SHA-256>`。宿主必须以禁止写入/删除共享的只读句柄打开固定的 `relay-profile.exe`，在该句柄上复核完整摘要，然后才启动，并保持句柄到进程结束。如果已有另一个 profile 正在使用此安装，需先关闭，再切换。安装或完整性检查失败时不能回退到原始程序。

## 无主机验证

在 Windows x64 上，使用 Python 3.11+ 和 Windows PowerShell 5.1，提供上述官方 ZIP 及一个测试用父目录：

```powershell
python tests/test_chiaki_profile.py --archive <官方便携包.zip> --work <测试用父目录>
```

测试只启用新生成的临时命名空间，实际调用 Qt 的 `list`。它向第一套配置加入一个**没有配对密钥或主机地址**的测试标签，核对另一套配置不可见，切回第一套时仍在；还检查两处名为 `Chiaki` 的字符串只改了正确的一处，并覆盖文件占用、原程序/派生程序损坏和注册表短名称冲突。测试结束只清理绑定匹配的测试命名空间。无需 PSN 登录、采集卡或真实 PS5；这个测试不验证视频帧或手柄串流。

## v0.1.9 姿态与播放配置

标准与 NR 模式共用相同命名空间。社区原版 EXE 的大小为 5,536,256 字节，SHA-256 为 `3488b8a2c6564f006b1447dc42b67e0fbafc5293963ee3af073a3f95e7db3e26`；组织/应用名共用文件偏移 `0x45d124` 的六个 `Chiaki` 字节。完整原版摘要固定后才派生，不信任其他可执行文件。社区源码提交与来源见 [NR 声明](../notices/CHIAKI-NR.md)。

[源码等价补丁](patches/dualsense-motion.patch)将四元数平滑死区设为零，并在 SDL 加速度事件中跳过人为重力偏移。保持 Madgwick 算法、原始传感器单位、原始加速度和角速度字段、串流包编码。参考上游 issue #777 与未合并 PR #805 的动作路径；不宣称等同 Sony 官方实现，也不包含该 PR 的其他功能。该修正作用于 SDL 运动控制器，去掉死区可能让小幅噪声更明显。

| 运行时 | 原始文件偏移 → 原始十六进制 | 替换值 |
| --- | --- | --- |
| 标准 v1.10.1 | `0x4823ec` → `3480b739`，`0x4823f0` → `3480373a`，`0x4823fc` → `3480b73a` | 各 4 字节 `00000000` |
| 标准 v1.10.1 | `0x66ce7` → `00`（SDL accel 分支的布尔参数） | `01` |
| NR 1.10.0-dlss5.7 | `0x44e6c4` → `3480b739`，`0x44e6c8` → `3480b7b9`，`0x44e6cc` → `3480373a`，`0x44e6d0` → `348037ba`，`0x44e6d4` → `3480b73a`，`0x44e6d8` → `3480b7ba` | 各 4 字节 `00000000` |
| NR 1.10.0-dlss5.7 | `0x65cfa` → `00`（SDL accel 分支的布尔参数） | `01` |

常量的代码引用已复核只来自对应的四元数函数。初始化器验证原版完整摘要、位置旧值，再替换；安装器恢复所有固定字节后重新匹配完整原版摘要。允许完整旧版六字节配置副本以便升级，但拒绝部分修正或未知改动。Rust 启动前仍独立匹配整个最终派生文件，并持有禁止写入/删除的锁。原始 `chiaki.exe` 不改，升级前已运行的连接窗口需先关闭才能替换副本。

初始化器仅在已绑定命名空间的默认配置和 `current_profile` 指定的配置中设置后台输入、缓冲和模式。不读取配对库/令牌字段。0.1.15 的 RelayRender 配置将旧设备缓冲迁移为 2,048 字节，原生引擎请求 512 个采样帧并通过设备回调读取一个有界 PCM 队列；应用队列通常最多 40 ms，硬件周期较大时自适应。旧标准/社区原版路径仍保留原有二次幂规范化规则。NR 模式请求 `neural_render=true`、`render_backend=vulkan`，首次默认不放大；普通模式清除 NR 请求。启动环境仅作用于串流子进程：WASAPI、SDL PS5 HIDAPI、后台事件、关闭 RawInput。它不改变系统音频格式或其他应用的手柄设置。

标准模式使用上文测试命令；NR 模式增加 `--community-nr` 并提供社区 ZIP。测试同时调用两种实际 EXE 中的纯四元数函数，验证 1 秒 `0.01 rad/s` 的连续小转动不会被吞掉，反向转动回到初始姿态；再验证 Qt 配置隔离、有效缓冲保留、不规则缓冲规范化、当前配置和凭证隔离。测试没有实际配对或连接主机。

## 原 Relay 连接面板

`src/connections.rs` 从完整绑定匹配的本项目命名空间导入现有配对，新增记录存入当前 Windows 用户的 DPAPI 文件。未读取或枚举其他软件的配对库。默认配置与当前选择的配置共用同步规则，运行时别名为 `Relay_<MAC十六进制>`，界面显示名称保存在加密文件中。空白、损坏或结构异常的存储不会被静默覆盖。

Qt 的 QByteArray 按 Qt 6.8 `qsettings.cpp` 的 `@ByteArray(...)` 包装：Latin-1 字节转 UTF-16；含 NUL 时使用不带终止符的 REG_BINARY，其余使用带终止符的 REG_SZ。已通过两种真实运行时的 `list` 读取 Rust 写入的合成配对记录。连接只传唯一别名与地址，不在命令行传配对密钥。

`RELAY_PLAYBACK_SETTINGS` 只接受有界的分辨率、帧率、编码、目标码率和 NR 布尔值；初始化器映射到 local/remote PS5 设置并读回核对，禁用手柄打开第二套菜单的快捷入口。实际串流使用 `stream` 子命令。发现和配对不启动后端 GUI。
