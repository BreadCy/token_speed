CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, directory TEXT, path TEXT, time_updated INTEGER);
CREATE TABLE model_usage (
  id INTEGER PRIMARY KEY, logical_request_id TEXT, session_id TEXT, turn_id TEXT,
  model_id TEXT, variant TEXT, agent TEXT, status TEXT,
  started_at INTEGER, first_token_at INTEGER, completed_at INTEGER,
  time_to_first_token_ms INTEGER, output_tokens INTEGER,
  input_tokens INTEGER, cache_creation_input_tokens INTEGER
);
INSERT INTO session VALUES ('sess-z-1', 'project-z-1', '/redacted/project', '/redacted/project/session', 3000);
INSERT INTO session VALUES ('sess-z-2', 'project-z-2', '/redacted/other-project', '/redacted/other-project/session', 6000);
INSERT INTO model_usage VALUES (1, 'req-z-1a', 'sess-z-1', 'turn-z-1', 'model-z', 'variant-z', 'zcode-agent', 'completed', 1000, 1200, 2500, 200, 40, 800, 0);
INSERT INTO model_usage VALUES (2, 'req-z-1b', 'sess-z-1', 'turn-z-1', 'model-z', 'variant-z', 'zcode-agent', 'completed', 1500, 1700, 3000, 200, 60, 900, 0);
INSERT INTO model_usage VALUES (3, 'req-z-incomplete', 'sess-z-1', 'turn-z-incomplete', 'model-z', NULL, 'zcode-agent', 'running', 4000, NULL, NULL, NULL, 0, 0, 0);
INSERT INTO model_usage VALUES (4, 'req-z-2', 'sess-z-2', 'turn-z-2', 'model-z', NULL, 'zcode-agent', 'completed', 5000, 5200, 6000, 200, 20, 700, 0);
INSERT INTO model_usage VALUES (5, 'req-z-mixed-a', 'sess-z-1', 'turn-z-mixed', 'model-a', NULL, 'zcode-agent', 'completed', 7000, 7100, 7500, 100, 10, 0, 0);
INSERT INTO model_usage VALUES (6, 'req-z-mixed-b', 'sess-z-1', 'turn-z-mixed', 'model-b', NULL, 'zcode-agent', 'completed', 7050, 7150, 7550, 100, 10, 0, 0);
INSERT INTO model_usage VALUES (7, 'req-z-missing', 'sess-z-1', 'turn-z-missing', 'model-z', NULL, 'zcode-agent', 'completed', 8000, NULL, 8500, NULL, 10, 0, 0);
INSERT INTO model_usage VALUES (8, 'req-z-partial-completed', 'sess-z-1', 'turn-z-partial', 'model-z', NULL, 'zcode-agent', 'completed', 9000, 9100, 9500, 100, 10, 0, 0);
INSERT INTO model_usage VALUES (9, 'req-z-partial-running', 'sess-z-1', 'turn-z-partial', 'model-z', NULL, 'zcode-agent', 'running', 9000, NULL, NULL, NULL, 0, 0, 0);
INSERT INTO model_usage VALUES (10, 'req-z-missing-model-a', 'sess-z-1', 'turn-z-missing-model', 'model-z', NULL, 'zcode-agent', 'completed', 10000, 10100, 11000, 100, 10, 0, 0);
INSERT INTO model_usage VALUES (11, 'req-z-missing-model-b', 'sess-z-1', 'turn-z-missing-model', NULL, NULL, 'zcode-agent', 'completed', 10000, 10100, 11000, 100, 10, 0, 0);
INSERT INTO model_usage VALUES (12, 'req-z-ttfb-a', 'sess-z-1', 'turn-z-ttfb', 'model-z', NULL, 'zcode-agent', 'completed', 12000, 12100, 12500, 100, 40, 0, 0);
INSERT INTO model_usage VALUES (13, 'req-z-ttfb-b', 'sess-z-1', 'turn-z-ttfb', 'model-z', NULL, 'zcode-agent', 'completed', 12600, NULL, 13000, NULL, 60, 0, 0);
