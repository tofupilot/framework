import time

def case_5_error():
    time.sleep(0.5)
    print("⚠ About to raise an exception...")
    raise RuntimeError("Unexpected error to demonstrate ERROR outcome")
