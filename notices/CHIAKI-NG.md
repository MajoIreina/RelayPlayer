# PS5 串流后端：chiaki-ng

RelayPlayer 的公开 Windows 包使用 [chiaki-ng](https://github.com/chiaki-ng/chiaki-ng) 的独立图形客户端提供 PS5 配对和 Remote Play。第一次连接时，播放器直接从该项目的 GitHub Releases 取得固定版本的完整 Windows x64 压缩包。RelayPlayer 的 GitHub 资产不镜像该压缩包，也不重新分发其中的 EXE、DLL 或 QML 文件。

| 项目 | 本版本采用的值 |
| --- | --- |
| 上游发布 | [v1.10.1](https://github.com/chiaki-ng/chiaki-ng/releases/tag/v1.10.1) |
| 对应提交 | [`f30da0aaeb96fc0026cc03683153f682352e7d7c`](https://github.com/chiaki-ng/chiaki-ng/tree/f30da0aaeb96fc0026cc03683153f682352e7d7c) |
| 官方 Windows x64 文件 | [chiaki-ng-win_x64-MSYS2-portable-1.10.1.zip](https://github.com/chiaki-ng/chiaki-ng/releases/download/v1.10.1/chiaki-ng-win_x64-MSYS2-portable-1.10.1.zip)（118,676,648 字节，约 113 MiB） |
| 压缩包 SHA-256 | `6ca3f5e6780608104d79345ab7755cfa77fefc33a41ae10651a874c43f54ea57` |
| 包内 `chiaki.exe` SHA-256 | `c69a1e2dbf7406f74ae8a33c7c98fba226d79431a36aca5bc7143cedc4d19dec` |

chiaki-ng 自身采用 **GNU AGPL v3，附 OpenSSL 链接例外**。许可、作者声明和源代码见该提交的 [COPYING](https://github.com/chiaki-ng/chiaki-ng/blob/f30da0aaeb96fc0026cc03683153f682352e7d7c/COPYING)、[LICENSES](https://github.com/chiaki-ng/chiaki-ng/tree/f30da0aaeb96fc0026cc03683153f682352e7d7c/LICENSES) 和[源码树](https://github.com/chiaki-ng/chiaki-ng/tree/f30da0aaeb96fc0026cc03683153f682352e7d7c)。运行库包括 Qt、FFmpeg、SDL、OpenSSL 等组件，各自保留其版权和许可。上游的 [Windows 构建流程](https://github.com/chiaki-ng/chiaki-ng/blob/f30da0aaeb96fc0026cc03683153f682352e7d7c/.github/workflows/build-msys2.yml) 和[运行库部署脚本](https://github.com/chiaki-ng/chiaki-ng/blob/f30da0aaeb96fc0026cc03683153f682352e7d7c/scripts/deploy-windows-msys2.sh) 列出了构建与收集依赖的方式。

## 下载、校验和离线准备

首次在线准备需要访问 GitHub，建议在播放器所在盘预留 **500 MB** 磁盘空间。准备工具把压缩包下载到 `backends/` 下的临时目录，在同一个文件句柄上校验 SHA-256 后解压，核对全部 **1,497 个文件**，成功后才将整套运行库移入 `backends/chiaki-ng/`。原始运行库约 301 MiB。以后每次打开新的连接窗口，都会重新校验这些原始组件；完好时不再下载。

为避免读取或修改独立安装的 chiaki-ng 的配对信息，RelayPlayer 在本机生成 `backends/chiaki-ng/relay-profile.exe`，使用独立的 Windows 用户配置名称。它包含配置名称的六字节替换，以及 v0.1.9 的小角度/重力参考修正。校验器只额外允许这一个普通文件，并确认恢复这些固定位置后与原版摘要一致；初始化器针对当前配置生成期望摘要，Rust 宿主再次比对，持有文件锁后仅启动该副本。宿主不会启动原版 `chiaki.exe`，两个 EXE 均不包含在 GitHub 的 RelayPlayer 资产中。[隔离实现、数据位置与复核方法](../docs/CHIAKI-PROFILE.md)说明了细节。

如果已识别的运行库损坏，关闭正在播放的窗口，再点同一连接按钮即可尝试修复。工具先下载（或读取指定的官方 ZIP）并完整校验新副本，然后才把原目录整体改名为同级 `backends/chiaki-ng.backup-<随机标识>`，在原位置启用新副本。备份中额外的文件和链接会原样保留。下载、空间或校验失败时原目录不动；切换失败会尝试恢复原目录，若另一个程序占用了原路径，则保留备份并报错。无法识别的陌生目录、充当目录的文件或重解析路径不会被移走。修复时需要在已有目录之外再预留约 **500 MiB**。准备工具不扫描或迁移任何配对记录。

普通用户直接点击播放器里的连接按钮即可。如果机器无法连接 GitHub，也可在别处取得上表中的**同一官方原包**，将 ZIP 复制到这台机器后，在 RelayPlayer 目录执行以下命令（替换 ZIP 路径），直接开始离线准备并打开连接窗口：

```powershell
.\RelayPlayer.exe --connect --backend-archive "D:\Downloads\chiaki-ng-win_x64-MSYS2-portable-1.10.1.zip"
```

如果只准备运行库，不想打开客户端或搜索主机，可以运行：

```powershell
powershell.exe -NoProfile -File .\tools\ensure-chiaki.ps1 -InstallRoot "$PWD\backends" -ArchivePath "D:\Downloads\chiaki-ng-win_x64-MSYS2-portable-1.10.1.zip"
```

已经准备好的运行库可以单独复核；以下命令只校验，不下载、不修复，也不启动客户端：

```powershell
powershell.exe -NoProfile -File .\tools\ensure-chiaki.ps1 -InstallRoot "$PWD\backends" -VerifyOnly
```

全部文件摘要的复算方法：取 ZIP 内 `chiaki-ng-Win/` 下的每个普通文件，去掉此前缀，保留相对路径中的 `/` 和大小写，按路径的序数顺序排序。每行以 `文件小写SHA256`、两个 ASCII 空格、`相对路径`、换行（LF）组成；这些行的 UTF-8 字节再做一次 SHA-256，应为 `f18019dca0124041de066cde3558c0d3ed7a9366ab37c30e2fce327ab3db506b`。这也会校验 DLL 与资源，不只是主程序。

可选 DLSS 5 NR 后端及模型另见 [NR 组件声明](CHIAKI-NR.md)。
