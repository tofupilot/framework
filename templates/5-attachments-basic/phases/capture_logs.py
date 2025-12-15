import tempfile


def capture_logs(attach, log):
    log.info("Generating log file...")

    with tempfile.NamedTemporaryFile(mode='w', delete=False, suffix='.log') as f:
        f.write("Test execution log\n")
        f.write("==================\n\n")
        f.write("Phase 1: Initialization - PASS\n")
        f.write("Phase 2: Calibration - PASS\n")
        f.write("Phase 3: Measurement - PASS\n")
        f.write("\nAll phases completed successfully\n")
        temp_path = f.name

    attach.file(temp_path, "execution.log")

    log.info("Log file attached")
