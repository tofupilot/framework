import sys


def case_3_fail(phase):
    print("Calling phase.fail() - FAIL outcome", file=sys.stderr)
    phase.fail()
