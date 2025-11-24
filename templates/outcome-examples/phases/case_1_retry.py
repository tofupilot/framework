import time


def case_1_retry(phase, run, measurements):
    time.sleep(0.5)

    measurements.count_retry = run.retry_count
    if run.retry_count >= run.retry_limit:
        measurements.leave_loop = True
        print(f"✓ Passed on retry {run.retry_count + 1}/{run.retry_limit + 1}")
        return

    print(f"⟳ Attempting retry {run.retry_count + 1}/{run.retry_limit}")
    phase.retry("Intentional retry to demonstrate behavior")
