import sys

def never_executed(phase):
    print("This should never be printed!", file=sys.stderr)
