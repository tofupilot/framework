import sys

attempt_count = 0


def case_1_retry(phase):
    global attempt_count
    attempt_count += 1

    print(f"Attempt {attempt_count}: Requesting retry...", file=sys.stderr)
    phase.retry()
