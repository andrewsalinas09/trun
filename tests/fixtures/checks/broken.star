# Test check with a runtime error (None + 1): must surface as a check error.
def check(run):
    run.metric("does_not_exist").avg() + 1
