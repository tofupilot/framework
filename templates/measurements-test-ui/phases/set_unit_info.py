import random


def set_unit_from_python(run, ui):
    """Minimalist phase that sets unit info with random values from Python."""

    # Generate random serial number (format: SN-XXXXX)
    serial_number = f"SN-{random.randint(10000, 99999)}"

    # Generate random part number (format: PN-XXXX)
    part_number = f"PN-{random.randint(1000, 9999)}"

    # Set unit information directly on the run context
    run.unit.serial_number = serial_number
    run.unit.part_number = part_number

    log.info(f"Unit info set: Serial={serial_number}, Part={part_number}")
