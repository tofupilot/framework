import time


def case_3_fail(phase):
    time.sleep(0.5)
    print("✗ Test failed - phase.fail() called")
    phase.fail("Deliberate failure to demonstrate behavior")
