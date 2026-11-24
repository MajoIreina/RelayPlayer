# 可选社区 DLSS 5 NR 组件

RelayPlayer 的 NR 模式参考 [windystrife/chiaki-ng-Ultimate](https://github.com/windystrife/chiaki-ng-Ultimate) 的 D3D12 桥接实现。固定发布 [1.10.0-dlss5.7](https://github.com/windystrife/chiaki-ng-Ultimate/releases/tag/1.10.0-dlss5.7)，源码提交为 `3c64539c7496bbd212122d2e0d7aff319bafb90e`。该项目继承 chiaki-ng 的 AGPL v3/OpenSSL 例外，另含适用各自许可的 Qt、SDL、FFmpeg、NVIDIA/AMD 组件，见其源码中的 COPYING、LICENSES、NOTICE.md 和运行时的 licenses 目录。本项目不把整个运行时或模型镜像到自己的发行资产。

| 项目 | 固定值 |
| --- | --- |
| 运行时 | `chiaki-ng-1.10.0-dlss5.7-win_x64-portable.zip`，271,677,844 字节 |
| ZIP SHA-256 | `b07e58000ebf38545518c955ad9462ad8fac30dc8e0438ff44d5119d93c91ebe` |
| 原始 EXE SHA-256 | `3488b8a2c6564f006b1447dc42b67e0fbafc5293963ee3af073a3f95e7db3e26` |
| 目录验证 | 1,438 文件、69 目录、629,530,922 字节；文件树 SHA-256 `096fa9ded30152d4d800fb5d8c48ef3c8175b7b764e7b9b5e0c3078e099efad4` |
| 模型发布 | [RankFTW/rhi-repo dlssnr-310.9.16](https://github.com/RankFTW/rhi-repo/releases/tag/dlssnr-310.9.16) |
| 模型 ZIP | `nvngx_dlssnr_310.9.16.zip`，192,378,839 字节 |
| 模型 ZIP SHA-256 | `dbeb1f2790b5c621887db99b1a56ac8bd5cb35207e8c074cbb806710f7460cab` |
| 模型 DLL | `nvngx_dlssnr.dll`，276,162,560 字节，版本 310.9.16.0 |
| DLL SHA-256 | `cdfa6c1ec784867182164c7836f324f087b5c9f374d0c9fe46f54999feabf3ed` |

模型是专有组件，不属于开源 DLSS SDK，也不是 RelayPlayer 开源代码。固定社区发布中的 DLL 没有 Authenticode 签名；本项目不能称其经过 NVIDIA 官方认证。用户主动选择 NR 模式后，按上游社区实现相同的来源下载并固定摘要校验，适用 NVIDIA 和该组件的许可条件。不会运行其安装套件、MFG Unlocker 或 AMD NeuralHub。本版只提供 NVIDIA RTX 路径；显卡/驱动不兼容时应返回普通模式，不应把其他算法标成 NR。

所有组件放在用户选择的解压盘 `backends/chiaki-nr`。每次启动核验完整运行时与模型；未知 DLL、旧版或已修改模型均拒绝。源码中有 `RELAY_NR_MODEL` 离线模型入口，供直接运行 `ensure-chiaki.ps1 -CommunityNr` 时使用，仍要求上述完整散列。宿主 NR 按钮总是使用已固定的自动准备流程。

独立配置副本和姿态修正是在接收方本机生成；与普通模式共享 RelayPlayer 的隔离命名空间，因此能复用配对记录。[配置与姿态改动](../docs/CHIAKI-PROFILE.md)列出具体偏移、来源和验证方法，原版 EXE/DLL 不修改。
