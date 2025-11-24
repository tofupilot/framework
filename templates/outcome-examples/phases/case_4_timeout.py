import time

def case_4_timeout():
    print("⏱ Starting long-running operation (will timeout)...")
    time.sleep(10)
    print("This line never executes - killed by timeout")
