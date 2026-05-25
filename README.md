# lava-cron

Typed `(deflava-cron …)` for scheduled lava deployments.

```lisp
(deflava-cron weekly-vpc-drift-check
  :expression "0 6 * * 1"
  :architecture aws-vpc-network
  :bindings (:name "prod" :cidr "10.0.0.0/16")
  :action plan)
```

Companion to lava-architectures. A cron-trigger (k8s CronJob /
GitHub Actions schedule / pangea-cron) invokes the architecture
render when the tick fires.

## Surface

- `CronSchedule { name, expression, architecture, bindings, action }`
- `Action = Plan | Apply | Destroy | Refresh`
- `schedules_in_source(src) -> Vec<CronSchedule>`
- `CronExpression::parse(expr)` — 5-field cron parser
- `schedules_firing_at(schedules, minute, hour, dom, month, dow)`

9/9 unit tests cover form extraction, missing-clause + unknown-action
errors, cron-field parsing (star + step + range + list), out-of-
range detection, schedule matching, serde round-trip.
