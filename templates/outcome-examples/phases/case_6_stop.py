import time

def case_6_stop(phase):
    time.sleep(0.5)
    print("⏹ User-initiated stop")
    phase.stop("Manual stop to demonstrate behavior")
