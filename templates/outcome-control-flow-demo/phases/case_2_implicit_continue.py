import sys


def case_2_implicit_continue(measurements):
    print("No explicit return - implicit CONTINUE", file=sys.stderr)
    measurements.sample_value = 42
