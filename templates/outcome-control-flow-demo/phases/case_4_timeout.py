import sys
import time

def case_4_timeout(phase):
    print("Starting long operation that will timeout...", file=sys.stderr)
    time.sleep(5)
    print("This line should never be reached", file=sys.stderr)
