# MEMORY

## 架构决策

- **聚合监控引擎**（2026-09-06 重设计检测逻辑）：`monitor.rs` 的 `spawn_engine(project)` 单线程管理全部已装 agent 源——每源一个 notify watcher（事件携带 agent 标记）、30s reconcile 全量重检测（新装/卸载自动出现）。对外只发 `EngineEvent::Statuses(Vec<AgentStatus>)`，UI **永不因切 agent 重启线程**（切 tab = 纯 UI 选择；只有改项目固定才 `restart_engine`）。
- **事件只重扫"脏" agent**：watcher 事件进 dirty 集，防抖后只重扫对应源；全量重扫真实大库（Claude projects 可达数百 MB）一次要几十 ms，事件驱动全量重扫会让 UI 更新退化到秒级——这是探针实测出的教训。
- **跟随模式** `config.follow_mode: manual|auto`（默认 manual，serde default 向后兼容）：auto 规则 = 当前对象生成中则保持、否则跟随最近活跃的生成中 agent、全部空闲停在最近活跃（不回跳默认）；**自动档中手动点 tab = 切回 manual 并锁定**（显式操作，不违背「永不静默切换」）。切模式不重启引擎，下一帧 drain 自动重选。
- **检测驱动的动态 tab**：`detect_all_installed()` 批量扫 4 个 agent（「已安装」= 数据源路径有效；activity_at 只做排序）。env 覆盖遵循 detect_source 的「不回退默认」原则：env 指错 = 该 agent 未安装，不影响其它 agent。**主卡 tab 行与固定项目输入行已移入设置抽屉**（2026-09-06）：主卡只留标题栏/大数字/三格/最近10轮/状态行（`MAIN_CLOSED_H` 403→321），Agent 选择=设置 02 行点选、「重新检测」按钮也在 02 节，固定项目=设置 03；主卡不再有任何 agent 切换与项目输入入口。
- **双平台一套 egui UI**：删除了 v0.5.9 的 Win32 死代码（float.rs/settings.rs/tray.rs，约 2000 行，早已不参与编译），windows-sys features 裁剪到 `Win32_Storage_FileSystem`（config.rs 原子替换用）。托盘用 tray-icon（跨平台）。**Windows 端从未视觉验证**——透明无边框窗口、悬浮球、托盘、字体需真机确认；编译门控走 CI。
- macOS 悬浮窗为 egui 无边框透明视口，采用**低饱和设计 token**：`#1D1E20 / #242529 / #191A1D / #EBEBEE / #9D9DA4 / #7FB98A / #D9AE5A`，弃用亮绿与青色。标题栏「设置」按钮常态底 `BTN_BG #2B2D32`（比表面浅半档、悬停再亮一档），文字 CENTER_CENTER 实测居中。
- **三形态同窗口切换**（`mode` + `ViewportCommand::InnerSize`，不拆多视口）：
  - 悬浮球 64×64 —— **启动默认态**；**双击 = 动效展开胶囊条**（`capsule_open`，自定义 250ms ease-in-out 三次曲线驱动窗口宽度 64⇄260（体宽，窗口另加两边 8px 边）——egui 内置 `animate_value_with_time` 是指数缓动，快起慢尾观感发闷，且动画期间每帧发 InnerSize 会让绘制与系统 resize 互追一帧，**逐帧 resize 透明窗口是 macOS 卡顿根源——最终方案：球/胶囊模式下窗口恒为 320×80（`BALL_WIN_W/H`，永不 resize），展开与收起是对称的 250ms ease-in-out 宽度动画（生长 64→260、收缩 260→64，都在固定画布内纯绘制，无淡入辅助——穿透架构落地后窗口永不 resize，收起不再需要瞬切/淡入补救）；透明区点击穿透靠 `cursor_watch` 线程：8ms 轮询全局光标（`CGEvent::location`，无需权限）× 球体屏幕区域（`viewport().outer_rect`），变化时经 wake channel 发 `SetInteractive` 切 `ViewportCommand::MousePassthrough`。副产物：球态窗口比可见球大，双击热区只限球体部分（穿透区点击落到桌面）**——收缩距离 196px；球/胶囊体外圈有 8px 透明边（`BODY_MARGIN`，窗口 80×80 / 292×80）——ping 光环半径最大 38.75px，无边会被窗口裁成残弧；**必须 `viewport.has_shadow = Some(false)`**（ViewportBuilder 公开字段，非 with_ 方法）——macOS 系统阴影沿窗口 alpha 轮廓绘制，光环外扩时会拖出一圈黑边，看起来像光环变黑）。悬浮感改用 egui 手动软阴影：球态 4 层半透明暗圆（alpha 38/26/15/7，半径 31..37，下移 2px）画在球体填充之下，随展开按 ball_alpha 淡出——收缩距离 212px——球/胶囊内容按宽度交叉淡入淡出（球内容 70→110 淡出、胶囊内容 100→150 淡入），同一全圆角胶囊形状）；再双击收回。**底色区分两态**：空闲球体 alpha 120（明显透底），随宽度线性回 185 到胶囊态，生成中恒实心 + 光环（`body_alpha` 插值）；状态点（球态右上 / 胶囊左端共用 `paint_status_dot`）活跃时与光环同频呼吸（1.6s：发射瞬间最亮最大 4.2px + 胀出 4.5→9px 微光晕，随拍衰减到 45% / 光晕散尽，空闲静态灰点）——胶囊态没有 ping 光环参照，光晕是呼吸可见性的主要载体，只有点本身缩放时肉眼几乎看不出。胶囊三段：`● 速度(21px, tok/s 顶对齐) │ 加权平均 │ Agent/状态`，宽 260，段间 1px LINE 两条 1px LINE 分割线：左线自适应贴速度尾部（+12px，钳制 120..140）；右线贴 Agent 段左缘（right-16-Agent段实测宽-12），且胶囊宽随 Agent 段自适应加宽（`capsule_w = CAPSULE_W + max(0, right_w-40)`，Claude Code 时约 302，左线右线位置不变、中段永远放得下）。迷你卡已删除（原暂停/展开入口移至主界面与托盘）；原「双击隐藏」手势随之移除，Mode 只剩 Main/Ball。
  - 主界面 400×403（最近 10 轮收起）/ 400×593（展开）
- 标题栏极简：状态点（绿=有数据、琥珀=生成中）+ Agent 名 + 右上角「设置」（画布直绘命中区，双击设置区不触发收起）；**双击标题栏 = 收起为悬浮球**；红绿灯、置顶开关、最小化均已移除（置顶常开）。项目行只有输入框 + 清空（设置已上移，project_row_widths 两列）。
- **设置抽屉**（2026-09-06 按高保真原型重做，只覆盖主卡不动主 UI）：打开时窗口 400×552（`SETTINGS_H`），CentralPanel 整屏画抽屉、关闭还原主卡。分组 01 跟随模式 / 02 跟随 Agent / 03 固定项目；大卡片单选（选中=绿描边+radio）+ Agent 行带「活跃中/空闲/未安装」状态；自动档下 Agent 列表降透明禁用。**暂存式交互**：`SettingsDraft` 打开时从 config 播种，任何改动亮「● 未保存」，保存才回写（项目路径非法则保持打开提示）；✕/取消/Esc 丢弃草稿。项目目录选择用 `rfd`（新增依赖）。**「03 / 固定项目」已整节移除**（2026-09-06，用户要求去掉该功能）：设置抽屉只剩 01 跟随模式 / 02 跟随 Agent，**取消/保存按钮也已移除——改为即时生效**（点选即写 config + save_config；SettingsDraft/settings_dirty/apply_settings_draft/settings_open 全部删除，Esc/✕ 仅关闭抽屉；提示文案已去掉，下方内容上移补位），**设置抽屉背景复用外层主面板的圆角卡片**（嵌套 CentralPanel 自带 frame 会把底角画成直角——alpha 实测 BL/BR=1、TL/TR=0，改用透明 frame 后四角对称），**滚动位置残留**：egui ScrollArea 按持久化 id 记住滚动偏移，抽屉关了再开会停在半滚动位置（卡片标题被视口顶边切成"排版乱"）——**列表表头（箭头/标题/轮数/分割线）的锚点必须用 head（allocate 的确定性矩形），不能用 ScrollArea 的 inner_rect（随滚动偏移与布局状态漂移，会把元素带出卡片左缘）**——"三角形位置有问题"即此因（content.left() 漂成负值把箭头带出卡片左缘）。修复 = 打开代数计数器 `settings_open_generation` 每次打开 +1 并绑进 `id_salt`，滚动位置随之归零；**滚动条 AlwaysHidden**（滚轮/拖拽仍可滚，条不再压列表行），rfd 依赖与 `restart_engine` 随之删除；`config.pinned_project` 字段保留（CLI 与采集仍支持，UI 不再提供入口）。rfd 依赖与 `restart_engine` 随之删除；`config.pinned_project` 字段保留（CLI 与采集仍支持，UI 不再提供入口）。**SETTINGS_H 552→450、SETTINGS_SCROLL_H 430→316**：滚动视口必须 ≤ 抽屉高减头部/底栏固定区，否则「保存/取消」底栏会被滚动区顶出窗外。表头（最近10轮）文字整体下移 2px（CJK 字形在 em 框内偏上，几何居中≠光学居中）；标题栏按钮文字同样光学下移 2px（实测墨迹上距 9/下距 12 → 修后 11/10）；列表行文字同样 +2px（行带中心 vs 墨迹中心实测差 3px，同一光学成因——**凡 CJK/数字混排行一律做 +2px 光学补偿**）；ghost_button（重新检测等）同款问题改画布直绘修正（egui Button 几何居中对 CJK 偏高，文字 +2px、宽=墨迹+20）；**装机后必须先 `pkill tokenspeed` 再 `open`**：单实例锁会让旧进程继续运行旧二进制，cp 换文件不重启=用户看到的还是旧 UI（2026-09-07 踩过）；
- **窗口尺寸自愈同步**（2026-09-07）：用户反复报告"展开时底部圆角变直角"，根因是 **InnerSize 的 resize 事件偶尔丢失**——窗口已 489 而 egui 仍按旧尺寸（实测 443）绘制卡片，底部 46px 连同圆角一起被切。旧逻辑只在 last_size 变化时发一次 InnerSize 永不重试；修复 = 每帧比对 `viewport().outer_rect` 与期望尺寸，误差 >1px 就重发（自愈）。**教训：egui/winit 的 resize 事件不可靠，涉及窗口尺寸的渲染 bug 先怀疑「请求尺寸≠实际尺寸」，用 outer_rect 比对而非截图猜**；
- **模型速度"无可靠区间"的真因**（2026-09-07）：一个 turn 由多个 LLM 请求行组成，ZCode 对部分行不写 first_token_at（实测 24h 内 37% 缺失），旧规则"任一行缺首 token 就整体判无可靠区间"导致该字段几乎恒空。修复 = 模型速度只用**有 first_token_at 的行**计算（同 turn 同模型，分段速度代表整体），精度据缺失比例降为估算（ttfb_rows==completed_rows 才算精确），全缺才显示无可靠区间。**教训：聚合指标遇到"部分样本缺字段"应降级而非整体放弃，否则字段等于报废**。
- **主模式窗口恒定 489（MAIN_MAX_H），卡片在窗口内伸缩**（2026-09-07，第三次"底部圆角被裁"报告后的架构级修复）：收起=卡片 307、展开=卡片 489（=窗口），设置=卡片 450，卡片下方透明区由光标线程穿透点击（interactive_rect 泛化为任意矩形，替代 ball_rect）。动机 = InnerSize resize 事件在部分环境会丢（三连报），逐帧自愈也救不了渲染/窗口的竞态——零 resize 才是根治。主卡片背景改为 CentralPanel 透明 frame + 自绘 rect_filled(BG, r10) + set_clip_rect(card)（嵌套 frame 底角直角问题一并消除）。卡片高度按实际窗口钳制（outer_rect.height），屏幕矮时列表视口收缩仍可滚。
- **窗口期望高度必须按屏幕钳制**（2026-09-07）：用户屏幕放不下 489 时 macOS 把窗口钳到 ~443，egui 仍按 489 绘制 → 底部连圆角被切（用户三次报告"展开时底部圆角不一样"的真因——开发者屏幕高放得下，永远无法复现）。修复 = `screen_available_height()`（主屏高-36）对 desired 高度取 min，放不下时列表视口收缩但仍可滚（AlwaysHidden 滚动条）。WINDOW_EDGE 提亮到 #4A4E56（#3C3F46 在暗背景仍不够辨）。表头悬停高亮跟随列表框圆角（展开圆顶、收起全圆角 6）。**注意：不能再用独立 `egui::Window` 做设置**——它画在 HUD 主窗口表面内，超过 400px 宽必被裁剪。
- **托盘**（`menubar.rs` + `tray-icon` 0.21 依赖）：左键单击 = 显示主界面；右键菜单 = 显示主界面 / 收起为悬浮球 / 退出。托盘必须在**事件循环运行后的 update() 首帧**创建——放 eframe 创建回调里 NSStatusItem 不显示（无报错、无窗口）。
- **单实例**：127.0.0.1:45170 TCP 锁；重复启动向已有实例发指令唤回主界面。
- **暂停/继续**（主界面与迷你卡共享）：暂停 = 跳过 `drain_updates` 冻结界面刷新，后台监听继续，状态行明示「已暂停 · 本地会话 …」。
- 主界面大数字 = 上一完整轮有效速度；副行带相对时间（`relative_time`，取 completed_at）；三格指标 = 模型速度（不可可靠时 —·无可靠区间）/ 最近一轮 / 会话加权平均；最近 10 轮默认收起（`recents_open`）。**展开高度动态**：`recents_list_height(n)` = 行数×26 封顶 182（=26×7 行整；封顶值必须取行高整数倍，190 那种非倍数会把第 8 行截成半行露在框外；少行贴合内容自适应，窗口高度经 `main_window_height(open, n)` 同步收缩）；表头文字与数据行共用同一列网格（锚 ScrollArea `inner_rect`），折叠箭头是矢量三角挂左 padding。表头下有**整条分割线**（展开时；列表项间分割线保留，首行顶线让位避免重线）；**ScrollArea 滚动偏移每帧吸附到行高整数倍**（`out.state.offset.y` snap 后 `State::store` 写回）：自由滚动的中间偏移会把最后一行截成半行、文字越过框底描边渲染到列表框外，吸附后视口边缘永远切在整行边界（平滑滚动动画期间可能短暂出现，落定即整行）；折叠箭头 `paint_caret` 两态体量相近（▾ 8×7 / ▸ 6×8）且整体下移 1px 与文字中线对齐（三角形质心偏高，不下移会看着浮起），中心挂 +4.5 拉开与文字到 3.5-4px 间距；**表头文字用 proportional 而非 monospace**——等宽字体数字相对汉字明显偏小、字距发空（proportional 栈 CJK 字体排最前，拉丁数字同走 Hiragino 天然对齐）。**双击主卡任意处收起圆球**（全局 `pointer.button_double_clicked`，非仅标题栏；排除标题栏按钮热区 `titlebar_btn_zones`（暂停+设置）与设置抽屉打开态）；**「暂停/继续」按钮在标题栏**（2026-09-06 自主卡状态行移入，设置左侧同款 BTN_BG 样式，状态行只留文字居中；两按钮热区同为 44×30——暂停 36×30 时与设置不等大被用户指出）；主卡常量 `MAIN_CLOSED_H=307`（295 会把内容顶穿窗口底缘，painted 圆角被盖成直角——高度收窄的下限是内容自然高度+底部 12px 边距）；已删除 `ACTION_W`、`ROW_GAP`（无消费方）；大数字旁 tok/s 下移 7px 对齐 x 高度中线；**列表行文字也用 proportional**（与表头同理，Menlo 数字与汉字光心不一致）；折叠箭头小体量（▾ 7×5.5 / ▸ 5×7），表头文字 +15 与箭头拉开 ~7px；标题栏状态点生成中时与 ping 同拍呼吸（`paint_status_dot` 已参数化颜色，球态/胶囊/标题栏三处共用），活跃时 Main 模式重绘节奏也提到 16ms。
- **混合 CJK+数字的文本慎用 monospace 家族**：Menlo 数字 x 高度小、字距宽，与回退 CJK 字体（占满 em 框）同排观感"忽大忽小"；表头/标签类文本用 proportional（CJK 在栈首，拉丁字形来自同一字体），纯数字表格列才保留 monospace。

## 踩坑与约束（长期有效）
- **"四个圆角不一样"排查结论（2026-09-06）**：窗口本体截图（`screencapture -l <windowid>` 带 alpha）实测四个极端角 alpha 全为 0——四角圆弧绘制始终一致（HUD_RADIUS 10）。"下角像直角"是**对比度错觉**：桌面底部是暗色，深色卡片 + 近隐形描边（LINE #2A2C30）让下角圆弧融进背景。修复 = 窗口描边专用更亮的 `WINDOW_EDGE #3C3F46`（仅 hud_frame 用，分隔线仍 LINE）。**教训：透明 HUD 判断圆角/直角必须读窗口 alpha 通道而非屏幕截图——背景色会污染颜色过滤，背后窗口的灰（如 IDE #2B2B2B）会冒充卡片色，曾把排查引向"内容绘制越界"的错误方向近一小时。**

- egui 0.32 没有假粗体（epaint 明确 TODO）：`.strong()` 不改变字重，粗体必须加载真实字面。Menlo.ttc 索引 0=Regular、1=Bold；Hiragino Sans GB.ttc 索引 0=W3、2=W6。大号数字注册自定义字族 `tsnum` = [menlo-bold, system-cjk]。
- 系统 CJK 字体缺 U+25BE（▾）等几何字形，会渲染成豆腐块：小三角用 `Shape::convex_polygon` 矢量绘制。
- egui 内容超出视口高度会被静默裁切（不会滚动主面板）：列表 ScrollArea 高度必须动态计算 `available_height - footer(≈34) - 卡片表头(≈26)`，不能用固定 max_height。**主界面窗口高度同理**：内容每加一行都要同步加窗口高度（用探针打印 `ui.min_rect().height()` 实测），溢出时 frame 的圆角被挤到窗口外，底部直接变方角。
- **`ui.horizontal` 内的 Frame 会继承水平布局**：卡片内容必须 `with_layout(Layout::top_down(..))` 显式切回垂直，否则所有子元素排成一行并被窗口裁掉（迷你卡按钮曾因此消失）。
- `with_layout(right_to_left/...)` 直接放进垂直布局会占据剩余全部矩形（内容垂直居中到整块空间）：行内左右对齐必须先包 `ui.horizontal`；在 ScrollArea 等嵌套容器里更不可控，表格行右对齐优先画布直绘 + `Align2::RIGHT_CENTER`。
- 无边框窗口必须 `.with_resizable(false)`：egui 默认 resizable(true)，macOS 会对无边框可缩放的透明窗口做系统级尺寸调整（实测静置时 620 逐帧收缩到 ~507）。
- 透明视口只设 `ViewportBuilder::with_transparent` 不够：eframe 默认 `App::clear_color` 是 rgba(12,12,12,180) 半透明深灰，整个窗口矩形会被刷上它，圆角外留下四个深色方块。必须重写 `clear_color` 返回 `[0.0, 0.0, 0.0, 0.0]`。
- 生成中状态用更快的重绘频率驱动动画（悬浮球 ping 光环 250ms、空闲 500ms），避免卡顿感。
- `cargo test` 与运行中的实例会竞争配置文件写入（config_tests），测试前先 `pkill tokenspeed`。
- 引擎相关测试的时间窗（`engine_options` 的 max_runtime）不能贴着理论下限设：刚跑完 release 构建/打包的高负载时段，70ms/120ms 窗口会偶发超时（连续两次复现），已放宽到 300ms/500ms/1000ms 后稳定。
- `painter.galley` 的锚点是**左上角**（`painter.text` 才能传 Align2）：需要垂直居中时必须回提 `galley.size().y / 2`，否则文字整体下沉半行。
- 引擎/检测相关测试必须用 `isolate_agent_env()`（monitor_tests.rs）把 ZCODE_HOME/XDG_DATA_HOME/CLAUDE_CONFIG_DIR 指到不存在的路径，否则会扫到本机真实 agent 数据——又慢（单次 collect 可达几十 ms）又不确定。

## 落地页 tokenspeed.html 的 headless 验证

- Chrome headless 最小布局视口约 500px：`--window-size=390` 时截图虽是 390px 宽，但布局按 500px 计算，会产生"内容溢出"的伪影。真实验证小屏用 iframe 包一层固定 390px 宽 + `--allow-file-access-from-files`，探针脚本把布局数据写进 `parent.document.title`，再 `--dump-dom` 读 title。
- CSS 覆盖规则必须放在基础规则之后（同优先级按源顺序取胜）；媒体查询里改 `.rowline` 这类后置定义的属性，规则要放在样式表末尾。
- 网格列 `1fr` 的最小尺寸是 auto（会被 nowrap 内容撑爆）：窄屏一律用 `minmax(0,1fr)`，并给子项 `min-width:0`。

## 视觉验证方法（本机实测）

- 窗口 bounds 用 Swift + CGWindowListCopyWindowInfo 按 owner 名取；截图用 `screencapture -x -R<x,y,w,h>`。
- `screencapture -l <windowid>` 对透明视口窗口会报 "could not create image from window"；区域截取可用，但菜单栏区域（y=0）的 -R 截取会稳定失败。
- 宿主终端有屏幕录制权限，但没有辅助功能授权：无法读 AX 树、无法合成点击；验证交互面板可临时改初始态构建（如 `show_main`/`recents_open`/`card_open`），验证后还原。
- egui 默认不暴露无障碍树（未接 AccessKit），computer-use 的元素定位不可用。
- 托盘图标是否存在于菜单栏，用 `tray.rect()` 查询（CGWindowList 的归属名会误报）。

## 用户背景

- 视觉基准 tokenspeed.html 与设计文档在 `~/Downloads/`（项目外，路径可能变化；如需长期引用建议 vendor 进仓库 docs/）。
- 产品原则：完成轮才计数、生成中不闪数、精确/估算文字化、单选跟随永不静默切换（自动跟随是用户显式开启的档位，且手动点 tab 即回手动档，不存在静默切换）、本地离线无遥测。
