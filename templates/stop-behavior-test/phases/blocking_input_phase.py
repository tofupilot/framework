def blocking_input_phase(run, ui):
    """Waiting for stdin input - will block on input()"""
    print("[Blocking Input] Waiting for user input from stdin")
    print("[Blocking Input] Try stopping this - input() blocks SIGTERM")
    try:
        response = input("Enter value: ")
        measurements.input_length = len(response)
        print(f"[Blocking Input] Received: {response}")
    except EOFError:
        print("[Blocking Input] Input interrupted")
