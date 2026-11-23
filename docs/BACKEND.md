# 播放后端接口 · v0.1.7

公开版负责窗口、音量、设置和状态呈现，另一个本地进程负责实际播放。本仓库没有 PS5 传输实现、采集驱动或 GPU 运行库。下面描述的是宿主与一个**兼容后端**如何对接；普通播放器或改名后的可执行文件不能直接替代它。

## 启动与退出

在 `RelayPlayer.exe` 同目录下放置 `backend/relay-backend.exe`。宿主通过绝对路径启动它，工作目录为播放器根目录。环境变量 `RELAY_ROOT`、`RELAY_DATA`、`RELAY_HOST_PID` 分别给出此实例的根目录、数据目录和宿主 PID。宿主不传命令行参数；`RELAY_PLAYER_ROOT` 只改变数据存放位置，不能指定可执行文件。请等到收到 `init` 后才启动辅助进程，此时后端已进入宿主管理的 Windows Job。控制消息只走标准输入和输出，不需要监听网络端口。

每条消息是单行 UTF-8 JSON，换行后刷新；标准输出只能写协议消息。收到 `exit` 或标准输入关闭时，后端须释放流、设备和窗口并退出。宿主结束时会清理其管理的子进程。

## 最小握手

宿主发出的首条请求示例：

```json
{"id":0,"name":"init","args":{}}
```

后端可回送结果，然后独立推送完整状态。后续请求也使用同一结构，请原样返回请求的 `id` 和 `name`。只有后端能接受命令时，才报告 `ready: true`。

```json
{"type":"result","id":0,"name":"init","value":{}}
{"type":"state","state":{"ready":true,"settings":{},"status":{"phase":"idle"}}}
```

例如，对未实现的操作应返回明确错误；不要回报成功或伪造帧统计：

```json
{"type":"error","id":3,"name":"devices","message":"本后端不支持枚举采集设备"}
```

`state` 是完整快照（宿主会替换上次状态）。`fatal` 加 `message` 表示不可继续的错误。可按能力支持 `discover`、`pair`、`connect`、`remote`、`capture`、`devices`、`settings`、`stop` 等请求。请求参数及界面的消费位置以 `src/panels.rs` 与 `src/main.rs` 为准。

## 视频、音量与画质反馈

- 提供嵌入式视频时，**由 `relay-backend.exe` 进程自身**创建可见的顶层 Win32 窗口（无所有者，类名 `RelayPlaybackWindow`），并从该进程输出音频。宿主以自己实际启动的进程 PID 查找此窗口和音频会话，不接受消息指定其他进程。
- 设置补丁如 `{"id":2,"name":"settings","args":{"bitrateKbps":75000}}` 表示请求 **75 Mbps**。后端应在确认后回送完整设置；如果参数只能在下次连接生效，应报告待重连设置。
- `status.phase: "streaming"` 配合 `status.stats` 报告实际帧。`width`、`height` 是处理前的输入尺寸；采集设备与输出尺寸有独立字段。`activeUpscaler`、`neuralRendering` 以及 `status.renderFeedback` 用于区分已选算法、实际算法和回退原因；没有测量时不要用目标值填充实测字段。

## 本地数据

仅使用宿主分配给当前实例的数据目录。配对或登录的输入不应出现在日志和状态快照里；只返回界面需要的字段。宿主、源码仓库与发布包均不需要预置真实账号、主机标识、配对码或授权文件。开发接入时请指向单独的 `RELAY_PLAYER_ROOT` 并使用虚构数据。
