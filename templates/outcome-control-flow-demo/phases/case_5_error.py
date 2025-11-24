import sys

def case_5_error(phase):
    print("About to raise an exception...", file=sys.stderr)
    raise ValueError("This is a test exception - ERROR outcome")
