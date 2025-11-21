import sys
import os
import time
from pathlib import Path


def attach_file_example(log, measurements, phase, attach):
    log.info("Attaching file to run")
    time.sleep(0.25)

    test_file = Path("/tmp/export_example.txt")
    test_file.write_text(f"Export attachment created at {time.time()}")

    attach.file(str(test_file), "export_example.txt")
    log.info(f"Attached file: {test_file}")

    data = b"Binary export data sample"
    attach.data(data, "export_data.bin")
    log.info("Attached binary data")

    measurements.files_attached = 2

    # Pass implicitly
