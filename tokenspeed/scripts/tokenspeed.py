#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""tokenspeed — ZCode 模型生成速度统计 (tokens/s)。

数据源: ~/.zcode/cli/db/db.sqlite 的 model_usage 表（SQLite 只读 URI 打开，
WAL 模式下与运行中的 ZCode 客户端互不阻塞、零写入）。

tokens/s = output_tokens / ((completed_at - first_token_at) / 1000)
只统计纯生成时段（不含首 token 等待）；first_token_at 缺失时回退
started_at 估算并在输出中用 "~" 标注，此时速度略偏低。

用法:
  py -3 tokenspeed.py                  人读统计: 最近一次 + 会话汇总 + 最近 10 次明细
  py -3 tokenspeed.py --limit 20       显示最近 20 次
  py -3 tokenspeed.py --session sess_x 只统计指定会话
  py -3 tokenspeed.py --hook           Stop hook 模式: 输出单行 {"additionalContext": "..."}
  py -3 tokenspeed.py --bench          分项耗时输出到 stderr
"""

import argparse
import json
import os
import pathlib
import sqlite3
import sys
import time
from datetime import datetime

T0 = time.perf_counter()
DEFAULT_DB = os.path.join(os.path.expanduser('~'), '.zcode', 'cli', 'db', 'db.sqlite')
FALSY = {'false', '0', 'off', 'no', 'none'}
MAX_SESSION_ROWS = 2000  # 单会话聚合上限，防止异常膨胀

ROW_SQL = """
SELECT model_id, variant, agent, session_id, started_at, first_token_at,
       completed_at, time_to_first_token_ms, output_tokens,
       CASE WHEN first_token_at IS NOT NULL THEN completed_at - first_token_at
            ELSE completed_at - started_at END AS gen_ms,
       CASE WHEN first_token_at IS NULL THEN 1 ELSE 0 END AS gen_estimated
FROM model_usage
WHERE status = 'completed' AND completed_at IS NOT NULL AND output_tokens > 0
"""


def fmt_secs(ms):
    if ms is None:
        return '-'
    s = ms / 1000.0
    if s < 60:
        return f'{s:.1f}s'
    return f'{int(s // 60)}m{int(s % 60)}s'


def fmt_ts(epoch_ms):
    return datetime.fromtimestamp(epoch_ms / 1000).strftime('%m-%d %H:%M:%S')


def fmt_model(model_id, variant):
    return f'{model_id} ({variant})' if variant else str(model_id)


def fmt_agent(agent):
    a = str(agent or '?')
    return 'main' if a == 'zcode-agent' else a.replace('zcode-', '', 1)


def speed(row):
    gen_ms = row[9]
    return row[8] * 1000.0 / gen_ms if gen_ms and gen_ms > 0 else 0.0


def latest_line(row):
    spd, gen_ms = speed(row), row[9]
    est = '~' if row[10] else ''
    parts = [
        f'{spd:.1f} tok/s',
        fmt_model(row[0], row[1]),
        f'{row[8]} tokens / {est}{fmt_secs(gen_ms)} 生成',
    ]
    if row[7]:
        parts.append(f'TTFT {row[7] / 1000.0:.1f}s')
    parts.append(fmt_ts(row[4]))
    parts.append(fmt_agent(row[2]))
    return ' | '.join(parts)


def connect_ro(db_path):
    uri = pathlib.Path(os.path.abspath(db_path)).as_uri() + '?mode=ro'
    return sqlite3.connect(uri, uri=True)


def fetch_session_rows(con, session_id):
    cur = con.execute(ROW_SQL + ' AND session_id = ? ORDER BY started_at DESC',
                      (session_id,))
    return cur.fetchmany(MAX_SESSION_ROWS)


def report_human(args, bench):
    if not os.path.exists(args.db):
        print(f'错误: 找不到数据库 {args.db}', file=sys.stderr)
        return 1
    con = connect_ro(args.db)
    bench['open'] = time.perf_counter()

    latest = con.execute(ROW_SQL + ' ORDER BY started_at DESC LIMIT 1').fetchone()
    if latest is None:
        print('没有找到已完成的模型请求数据。')
        return 0

    session_id = args.session or latest[3]
    rows = fetch_session_rows(con, session_id)
    bench['query'] = time.perf_counter()
    con.close()

    if not rows:
        print(f'会话 {session_id} 没有已完成的模型请求数据。')
        return 0

    head = rows[0]
    print(f'⚡ 最近一次: {latest_line(head)}')

    n = len(rows)
    total_out = sum(r[8] for r in rows)
    total_gen = sum(r[9] for r in rows)
    est_cnt = sum(r[10] for r in rows)
    avg = total_out * 1000.0 / total_gen if total_gen > 0 else 0.0
    sid_short = session_id if len(session_id) <= 22 else session_id[:19] + '…'
    print(f'📊 会话 {sid_short} 汇总: {n} 次请求 | 加权平均 {avg:.1f} tok/s | '
          f'共输出 {total_out:,} tokens / 生成 {fmt_secs(total_gen)}')
    if est_cnt:
        print(f'   （其中 {est_cnt} 次缺首 token 时间，速度按总时长估算）')

    limit = max(1, args.limit)
    print(f'📋 最近 {min(limit, n)} 次:')
    print(f'  {"TIME":<14} {"MODEL":<20} {"TOK/S":>6} {"OUT":>7} {"GEN":>7} '
          f'{"TTFT":>6} {"AGENT":<10}')
    for r in rows[:limit]:
        est = '~' if r[10] else ' '
        ttft = f'{r[7] / 1000.0:.1f}s' if r[7] else '-'
        print(f'  {fmt_ts(r[4]):<14} {fmt_model(r[0], r[1]):<20} {speed(r):>6.1f} '
              f'{r[8]:>7} {est}{fmt_secs(r[9]):>7} {ttft:>6} {fmt_agent(r[2]):<10}')
    return 0


def report_hook(args, bench):
    if args.auto_report.strip().lower() in FALSY:
        return
    event = {}
    if not sys.stdin.isatty():
        try:
            event = json.loads(sys.stdin.read() or '{}')
        except Exception:
            event = {}

    con = connect_ro(args.db)
    bench['open'] = time.perf_counter()
    session_id = event.get('session_id')
    row = None
    if session_id:
        rows = fetch_session_rows(con, session_id)
        if rows:
            row = rows[0]
    if row is None:
        row = con.execute(ROW_SQL + ' ORDER BY started_at DESC LIMIT 1').fetchone()
    bench['query'] = time.perf_counter()
    con.close()
    if row is None:
        return

    spd, gen_ms = speed(row), row[9]
    est = '~' if row[10] else ''
    text = (f'⚡ {spd:.1f} tok/s | {fmt_model(row[0], row[1])} | '
            f'{row[8]} tokens / {est}{fmt_secs(gen_ms)}')
    if row[7]:
        text += f' | TTFT {row[7] / 1000.0:.1f}s'
    print(json.dumps({'additionalContext': text}, ensure_ascii=False))


def main():
    parser = argparse.ArgumentParser(
        prog='tokenspeed', description='ZCode 模型生成速度统计 (tokens/s)')
    parser.add_argument('--limit', type=int, default=10,
                        help='明细表显示最近几次（默认 10）')
    parser.add_argument('--session', help='只统计指定 session_id（默认取最近活动的会话）')
    parser.add_argument('--db', default=DEFAULT_DB, help='db.sqlite 路径')
    parser.add_argument('--hook', action='store_true',
                        help='Stop hook 模式: 输出单行 {"additionalContext": "..."}')
    parser.add_argument('--auto-report', default='true',
                        help='hook 模式开关，false/0/off 时静默退出')
    parser.add_argument('--bench', action='store_true', help='输出分项耗时到 stderr')
    args = parser.parse_args()

    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding='utf-8', errors='replace')
        except Exception:
            pass

    bench = {}
    try:
        if args.hook:
            try:
                report_hook(args, bench)
            except Exception:
                pass  # hook 模式任何失败都静默退出，绝不阻塞对话
            rc = 0
        else:
            rc = report_human(args, bench)
    finally:
        if args.bench:
            end = time.perf_counter()
            marks = [('startup', T0), ('open', bench.get('open')),
                     ('query', bench.get('query')), ('end', end)]
            for (name, t), (_, nxt) in zip(marks, marks[1:]):
                if t is not None and nxt is not None:
                    print(f'[bench] {name:>8}: {(nxt - t) * 1000:7.1f} ms',
                          file=sys.stderr)
            print(f'[bench]   total: {(end - T0) * 1000:7.1f} ms', file=sys.stderr)
    return rc


if __name__ == '__main__':
    sys.exit(main())
