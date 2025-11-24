import time

def case_7_skip(phase):
    time.sleep(0.5)
    print("⏭ Skipping this phase")
    phase.skip("Optional phase skipped")
