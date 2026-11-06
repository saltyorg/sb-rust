# The Saltbox Docker controller's HTTP API

What `sb docker start`, `stop` and `restart` rely on, verified by reading
the controller's source (Saltbox Docker controller v1.0.8, 2026-10-21;
Go, chi v5.5.0 router) on 2026-11-06. Not run against a live controller.

- **The routes sb uses:** `POST /start`, `POST /stop`, `GET
  /job_status/{job_id}` (the router also has `GET /ping`, `GET /ready`,
  `POST /block`, `POST /block/{duration}` and `POST /unblock`). A trailing
  slash or an extra segment is a plain-text 404; a wrong method is chi's
  405.
- **No redirects:** no slash-redirect middleware, no explicit redirect;
  an unmatched path falls back to `http.NotFound`.
- **`ignore`:** repeated query values (commas inside a value split, items
  trimmed, empty ones dropped) are merged with a JSON body's `ignore`
  array, used as sent. The optional JSON body (at most 64 KiB, one JSON
  document, unknown fields refused, each with 400) can also carry
  `timeout` and, for stop only, `shutdown`; an empty body is accepted. On
  start, an ignored container isn't started and its unavailable
  dependents are `protected`; on stop, containers an ignored one needs
  are `protected`.
- **One job at a time:** jobs run in order from a queue of 100; a full
  queue is 503. A start can wait `pending` behind a stop.
- **Answers:** job creation is 200 with `{"job_id": "<uuid>", "status":
  ...}`; a job's status is one of `pending`, `running`, `completed`,
  `failed`. An unknown or expired ID is 404 with `{"status":"not_found"}`.
  Finished jobs are kept about an hour (cleanup every 5 minutes), fewer
  when over 1000 finished jobs are held. Bad requests are 400, a blocked,
  full or shutting-down controller 503, other failures 500, each with
  `{"error": ...}`.
- **Container failures fail the job:** a container that failed, was
  blocked or was cancelled makes the job `failed`, its `error` "one or
  more containers did not reach the requested state" and its `failed` and
  `outcomes` naming them; `completed` means every container reached the
  requested state (or was ignored or protected).
- **No cancel endpoint:** a job runs on its own context, so it goes on
  whether or not a client polls. A shutdown-intent stop or the
  controller's own shutdown cancels the active start and fails queued
  ones; a running stop is cancelled only past the shutdown's 30 s
  deadline. Each job also has its own timeout (defaults 600 s start,
  300 s stop). A cancelled job reads `failed`.
- **No authentication;** the listen address defaults to loopback and is
  configurable.
