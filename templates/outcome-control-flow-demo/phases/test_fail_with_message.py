import sys


def test_fail_with_message(phase):
    print("Testing phase.fail with message", file=sys.stderr)
    phase.fail("This is a test failure message")
