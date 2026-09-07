---
name: tokenspeed
description: 查询 AI 编码工具的模型生成速度统计（tokens/s、首 token 延迟 TTFT、输出 token 量），支持 ZCode、Codex、OpenCode、Claude Code、Pi。当用户询问生成速度多快、tokens per second、tok/s、速度统计、模型输出快不快等与模型输出速度相关的问题时使用。
---

运行 tokenspeed 插件自带的统计程序（单文件 exe，无需任何运行时），把结果整理成简洁表格回复用户：

```bash
EXE=$(ls -1 /c/Users/czh/.zcode/cli/plugins/cache/*/tokenspeed/*/bin/tokenspeed.exe 2>/dev/null | sort -V | tail -1)
[ -z "$EXE" ] && EXE="/e/zcode_plugin/tokenspeed/bin/tokenspeed.exe"
"$EXE" --limit 10
```

- 用户指定看某个工具时追加 `--tool`：`zc`=ZCode、`cx`=Codex、`oc`=OpenCode、`cc`=Claude Code、`pi`=Pi。
- exe 不存在时回退：`py -3 E:/zcode_plugin/tokenspeed/scripts/tokenspeed.py --limit 10`（仅支持 ZCode）。
- 输出中 `~` 前缀表示该条速度按本地会话记录估算（含工具执行时间）；ZCode（ZC）为精确值。
- 某工具显示"（无本地会话数据）"表示本机没有它的会话记录，如实转述。
- 只转述程序输出的真实数据，不要编造数值。
