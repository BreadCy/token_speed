# tokenspeed — AI 编码工具生成速度插件

显示模型生成速度（tokens/s）。**不止 ZCode**：同时读取本机 Codex、OpenCode、Claude Code 的本地会话记录，统一展示。

| 方式 | 用法 | 特点 |
| --- | --- | --- |
| **置顶悬浮条** | 双击 `tokenspeed.exe`（或 `--float`） | **零会话占用**：双行设计悬浮条置顶显示（工具色标 + 大号速度 + 模型/均值细节），自动跟随"最近有输出的工具"换色换内容；附带**托盘图标**（切工具/开机自启/退出）。最接近"状态栏"的形态 |
| 斜杠命令 | `/tokenspeed [次数] [工具]` | 按需查询：各工具最近一次 + ZCode 会话汇总 + 最近 N 次明细表 |
| 自然提问 | "现在速度多快" / "codex 多快" | skill 自动触发，等价于斜杠命令 |
| 实时终端 | `tokenspeed.exe --watch` | **零会话占用**：终端内单行刷新，不想用悬浮窗时的替代 |
| 每轮自动上报 | Stop hook（当前版本无实际显示效果） | 受 ZCode 实现限制，Stop hook 的上下文不会展示（详见"已知限制"），保留作前向兼容 |

## 支持的工具与数据精度

| 标识 | 工具 | 本地数据 | 精度 |
| --- | --- | --- | --- |
| `ZC` | ZCode | `~/.zcode/cli/db/db.sqlite`（model_usage 表，只读） | **精确**：只统计纯生成时段，另含 TTFT、子代理标注 |
| `CX` | Codex | `~/.codex/sessions/**/rollout-*.jsonl`（token_count 事件） | 估算 `~`：相邻事件时间差，含中间工具执行 |
| `OC` | OpenCode | `~/.local/share/opencode/opencode.db`（message 表） | 估算 `~`：消息 created→completed 覆盖整轮 |
| `CC` | Claude Code | `~/.claude/projects/**/*.jsonl`（assistant usage） | 估算 `~`：相邻 assistant 时间差 |

- `~` 前缀 = 按本地记录估算，时长里混入了工具执行/空闲时间，速度**略偏低**，仅供横向对比。
- 某工具本机没有会话记录时显示"（无本地会话数据）"。
- 悬浮条与 `--watch` 自动显示"最近有输出的工具"（比如刚在 Codex 里跑完一轮，就显示 CX）。

## 安装

**方式 A：独立 app（推荐，与 ZCode 插件无关）**

已安装到 `C:\Users\czh\AppData\Local\Programs\tokenspeed\`（含 tokenspeed.exe、README.txt、uninstall.bat），开始菜单和桌面各有一个 **tokenspeed** 快捷方式，双击即启动悬浮条+托盘。单实例：重复双击只会唤起已有悬浮条。卸载运行 `uninstall.bat`。

重新构建/升级：`cd /e/zcode_plugin/tokenspeed-rs && cargo build --release`，然后把 `target/release/tokenspeed.exe` 复制到上面的安装目录覆盖。

**方式 B：作为 ZCode 插件**

1. 打开 ZCode 客户端 → 设置 → 插件管理 → 发现/市场，点 `+` 添加本地目录 `E:\zcode_plugin`（该目录是一个本地 marketplace，见 `marketplace.json`）。
2. 安装 **tokenspeed** 插件，新开会话生效，获得 `/tokenspeed` 命令与提问触发。

无任何运行时依赖：统计程序是单个独立 `bin/tokenspeed.exe`（Rust 编译，静态内嵌 SQLite）。

## 速度怎么算

```
tokens/s = output_tokens / 生成时长
```

- **ZCode（精确）**：数据来自 `~/.zcode/cli/db/db.sqlite` 的 `model_usage` 表（只读打开，WAL 模式下与运行中的客户端互不阻塞），时长取 `completed_at − first_token_at`，即纯生成时段；首 token 延迟（TTFT）单独列出。明细里 `~` 表示该次缺 `first_token_at`，按总时长估算。子代理（如 `zcode-Explore`）单独标注。
- **其余工具（估算）**：只能从会话日志反推。Codex 取相邻 `token_count` 事件的间隔；OpenCode 取 assistant 消息 created→completed；Claude Code 取相邻 assistant 消息间隔。超过 30 分钟的间隔视为跨轮空闲，直接丢弃。
- 只统计有输出的请求。

## 配置

插件设置里的 **auto_report（每轮自动上报速度）** 默认开启；关闭后 Stop hook 静默退出，仍可随时 `/tokenspeed` 查询。

## 程序直用

```bash
E:/zcode_plugin/tokenspeed/bin/tokenspeed.exe                   # 无参数 = 悬浮条 + 托盘（双击等同）
E:/zcode_plugin/tokenspeed/bin/tokenspeed.exe --report          # 人读统计报表（终端用）
E:/zcode_plugin/tokenspeed/bin/tokenspeed.exe --tool cx         # 只看 Codex (zc/cx/oc/cc)
E:/zcode_plugin/tokenspeed/bin/tokenspeed.exe --limit 20        # 最近 20 次
E:/zcode_plugin/tokenspeed/bin/tokenspeed.exe --session sess_x  # 只统计指定 ZCode 会话
E:/zcode_plugin/tokenspeed/bin/tokenspeed.exe --watch           # 终端实时单行（默认 2s 刷新）
E:/zcode_plugin/tokenspeed/bin/tokenspeed.exe --watch 1 --tool cx
E:/zcode_plugin/tokenspeed/bin/tokenspeed.exe --autostart=on    # 开机自启悬浮条（off 取消）
E:/zcode_plugin/tokenspeed/bin/tokenspeed.exe --bench           # 分项耗时（四源合计约 20ms）
```

**悬浮条与托盘**：无边框半透明置顶小窗，双行显示，左键按住拖动，右键关闭。托盘图标是随工具换色的"速度柱状图"（可能在任务栏"隐藏图标"^ 里，可拖出常驻）：

- 托盘左/右键弹菜单：**切换工具**（全部/ZCode/Codex/OpenCode/Claude Code）、**开机自启**（写 HKCU 注册表 Run 键，`--autostart=on|off` 等价）、**设置…**（修改四个工具的数据路径）、**退出**；
- 托盘图标双击：隐藏/显示悬浮条；
- 悬浮条本体随过滤即时换色换内容。

**数据路径设置**：托盘菜单"设置…"打开深色主题的设置窗口。首次打开会**自动检测本机已安装的 Code Agent**（按 `ZCODE_HOME` / `CODEX_HOME` / `XDG_DATA_HOME` / `CLAUDE_CONFIG_DIR` 环境变量与用户主目录固定位置逐个探测，数据库文件还会校验 SQLite 文件头），只列出检测到的工具；没检测到的（未安装）不出现在列表里。

- 列表可**收起**：点「▾ 路径列表 N」标题行折叠整块列表（窗口随之变矮），收起时显示各工具彩色摘要；再次点击展开；
- 每行：工具色圆点 + 名称 + 路径 + **浏览…**（文件/文件夹选择器）+ **上移 / 下移 / 删除**——顺序即报表里"各工具最近一次"的排列，删除的工具不再被统计；
- **检测**按钮探测本机候选路径，检测过即变为**重新检测**（把删掉的行按最新检测结果加回来）；按过检测后才出现**恢复默认**（恢复到检测到的路径）；
- 路径无效（不存在 / 不是有效的 SQLite 文件）时该行文字变红并在底部提示；修改路径即时校验；
- 窗口高度随行数自适应（增删行、折叠/展开即时变高变矮）；空列表显示"未检测到已安装的 Code Agent"引导；
- 保存写入注册表 `HKCU\Software\tokenspeed`（`agents` 有序列表 + 各工具路径），下一秒刷新立即生效，无需重启。`--db` 参数仍优先于设置里的 ZCode 路径。

启动瞬间闪过的黑色控制台窗口会自动消失，属正常现象。

## 零会话占用说明

聊天里显示的任何内容都会成为会话上下文——这是机制本身决定的。因此：

- `/tokenspeed` 每次约耗 1.5K tokens（命令模板 + 程序输出 + 回复表格），写入后随会话持续携带；
- **`--watch` / `--float` 与终端直调完全在会话之外，零占用**；
- Stop hook 同样零占用（输出当前会被丢弃）。

## 源码与重新构建

Rust 源码在 `E:\zcode_plugin\tokenspeed-rs\`（不随插件安装复制）。修改后重新构建并同步两处：

```bash
cd /e/zcode_plugin/tokenspeed-rs && cargo build --release
cp target/release/tokenspeed.exe ../tokenspeed/bin/
cp target/release/tokenspeed.exe /c/Users/czh/.zcode/cli/plugins/cache/zcode-plugin-local/tokenspeed/0.1.0/bin/
```

依赖 MSVC 工具链（VS2022 C++ 工作负载 + Windows SDK，rusqlite bundled 需编译 SQLite C 源码）。

## 后备版本

`scripts/tokenspeed.py` 是等价的 Python 实现（仅支持 ZCode 数据源），供无 Rust 工具链时自行构建使用：`py -3 scripts/tokenspeed.py`。插件本身不依赖它。

## 平台支持

- **Windows**：完整功能（悬浮条、托盘、自启、watch、报表）。独立 app 安装于 `%LOCALAPPDATA%\Programs\tokenspeed`，exe 已内嵌应用图标与版本信息。
- **macOS / Linux**：统计核心是纯 Rust（SQLite/JSONL 读取路径基于 `$HOME`，天然跨平台），`--report` / `--watch` / `--hook` 可直接编译使用；悬浮条和托盘用的是 Win32 API，Mac 上需要另行实现（Cocoa），且必须在 Mac 机器上编译。目前未提供现成的 Mac 二进制。

## 已知限制与排障

- ZCode 没有状态栏机制，无法显示"流式输出中的实时速度"，最细粒度是"每次模型请求的平均生成速度"。
- **每轮自动上报为什么"看不见"（已在 zcode.cjs 实现层确认）**：ZCode 对 Stop hook 的 `additionalContext` 只在 hook 同时返回 `continue: true`（即强制模型继续运行，最多 3 次）时才注入消息历史；只返回 `additionalContext` 会被直接丢弃，UI 也不会渲染。这是框架的"阻止停止"机制，不是显示通道。因此本插件的 hook 只是前向兼容地存在（每轮多一次约 10ms 的 exe 调用，无实际效果）；**查看速度请用 `--float` 悬浮条或 `/tokenspeed`**。
- Codex/OpenCode/Claude 的速度为估算值：其本地记录没有"首 token 时间"，时长只能包含工具执行时间，数值偏低是预期行为。
- Claude Code 显示无数据：本机 `~/.claude` 下没有 `projects/` 会话记录（未在该工具里产生过对话）。适配器按官方标准格式编写，产生会话后自动可见。
- hook 无上报：确认插件安装后 `bin/tokenspeed.exe` 存在；可手动运行看报错。
- 速度为 0 或无数据：`~/.zcode/cli/db/db.sqlite` 不存在（如使用便携版/自定义数据目录），可用 `--db` 指定路径，或在托盘"设置…"里永久修改。
