# Test check: stop the run when it reports too many errors.
META = {"every": 1, "kill": True}

def check(run):
    errors = run.metric("errors").last()
    if errors != None and errors >= 3:
        fail("error budget exhausted: %d errors" % errors)
    elif errors != None and errors >= 1:
        warn("errors reported: %d" % errors)
