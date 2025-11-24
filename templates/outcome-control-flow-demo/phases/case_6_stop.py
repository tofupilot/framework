import sys

def case_6_stop(phase):
    print("Calling phase.stop() - STOP outcome", file=sys.stderr)
    phase.stop()
