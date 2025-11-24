import sys

def case_9_fail_with_stop(phase):
    print("Calling phase.fail() with then.fail: stop override", file=sys.stderr)
    phase.fail()
