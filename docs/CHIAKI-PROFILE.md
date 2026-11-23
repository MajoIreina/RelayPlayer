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

两个 RIP 相对 `lea` 都指向只读段的 RVA `0x223dbb`（文件偏移 `0x221dbb`），构造参数时都使用长度 6，并经 `QString::fromUtf8` 转换。偏移 `0x22af6d` 的另一个 `Chiaki` 由 QML 单例使用。初始化脚本固定完整原文件摘要，并复核指令目标、导入槽和相邻字节，只替换前一处的六个字符，连结尾 NUL 和 PE 头都不改。串流协议、账户授权、QML、亲自配对的要求以及 DLL 均无改动。上游更新时必须重新审核；不得移除摘要检查后直接使用此方法。

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
