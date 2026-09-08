CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, directory TEXT, path TEXT, time_updated INTEGER);
CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
INSERT INTO session VALUES ('sess-oc-1', 'project-oc-1', '/redacted/opencode-project', '/redacted/opencode-project/session', 3000);
INSERT INTO message VALUES ('msg-oc-user', 'sess-oc-1', 500, 500, '{"role":"user","parts":[]}');
INSERT INTO message VALUES ('msg-oc-a', 'sess-oc-1', 1000, 2000, '{"role":"assistant","modelID":"model-oc","providerID":"provider-oc","tokens":{"input":10,"output":20},"time":{"created":1000,"completed":2000},"finish":"tool-calls"}');
INSERT INTO message VALUES ('msg-oc-tool', 'sess-oc-1', 2000, 2000, '{"role":"user","parts":[{"type":"tool_result"}]}');
INSERT INTO message VALUES ('msg-oc-b', 'sess-oc-1', 2000, 3000, '{"role":"assistant","modelID":"model-oc","providerID":"provider-oc","tokens":{"input":10,"output":30,"cache":{"read":400,"write":50}},"time":{"created":2000,"completed":3000},"finish":"stop"}');
