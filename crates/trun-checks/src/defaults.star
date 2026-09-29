# Built-in default checks (docs/05-checks.md).
#
# Thresholds come from [defaults.checks] in $TRUN_HOME/config.toml via run.config().
# Durations are seconds. To replace this file entirely, put a checks/defaults.star in
# your project's .trun/ or in $TRUN_HOME; to turn defaults off for one run, use
# `trun run --no-default-checks`.

META = {"applies": "*", "every": secs(5)}

def check(run):
    if run.lifecycle != "running":
        return

    # Silence: no output, heartbeat, or structured event for too long.
    # `::expect silence=20m` raises the limit for legitimately quiet phases.
    limit = run.expect_silence() or run.config("silence", mins(5))
    quiet = run.silence()
    if quiet != None and quiet > limit:
        stalled("no output for %s (limit %s)" % (fmt_duration(quiet), fmt_duration(limit)), key="silence")

    # No progress: a step that reports progress stopped moving.
    no_progress = run.config("no_progress", mins(15))
    for s in run.steps():
        if s["running"] and s["current"] != None:
            age = run.progress(s["id"]).age()
            if age != None and age > no_progress:
                stalled("step '%s' has not progressed for %s (at %s)" % (s["name"], fmt_duration(age), s["current"]), key="no-progress:" + s["id"])

    # Zombie: the process is alive but idle, silent, and not using a GPU.
    zombie = run.config("zombie", mins(10))
    cpu = run.proc_cpu()
    gpu = run.gpu_util().max(zombie)
    if (run.elapsed() > zombie and cpu.count(zombie) >= 2 and cpu.max(zombie) < 1.0
            and quiet != None and quiet > zombie and (gpu == None or gpu < 5)):
        stalled("process idle for %s: no CPU, no output%s" % (fmt_duration(zombie), "" if gpu == None else ", no GPU"), key="zombie")

    # NaN / inf metrics: stays failing for the window after the last bad value.
    nan_window = run.config("nan_window", mins(5))
    for name in run.metrics():
        bad = run.metric(name).count_non_finite(nan_window)
        if bad > 0:
            fail("metric '%s' had %d NaN/inf value%s in the last %s" % (name, bad, "" if bad == 1 else "s", fmt_duration(nan_window)), key="nonfinite:" + name)

    # Disk space on the volume holding the working directory.
    free = run.disk_free().last()
    if free != None:
        if free < run.config("disk_fail_bytes", 200e6):
            fail("disk nearly full: %d MB free" % (free / 1e6), key="disk")
        elif free < run.config("disk_warn_bytes", 2e9):
            warn("disk space low: %.1f GB free" % (free / 1e9), key="disk")

    # Memory pressure on the host (sustained).
    mem = run.host_mem()
    mem_window = run.config("memory_window", mins(2))
    low = mem.min(mem_window)
    if low != None and mem.count(mem_window) >= 3 and low > run.config("memory_pct", 95):
        warn("host memory above %d%% for %s (out-of-memory kill likely)" % (run.config("memory_pct", 95), fmt_duration(mem_window)), key="memory")

    # GPU idle (only when GPU samples exist).
    gpu_idle = run.config("gpu_idle", mins(10))
    util = run.gpu_util()
    top = util.max(gpu_idle)
    if top != None and util.count(gpu_idle) >= 3 and run.elapsed() > gpu_idle and top < 5:
        stalled("GPU idle for %s (dataloader bottleneck or hang?)" % fmt_duration(gpu_idle), key="gpu-idle")
