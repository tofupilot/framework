import random


def set_unit_from_python(run, ui):
    """Minimalist phase that sets unit info with random values from Python."""

    serial_number = f"SN-{random.randint(10000, 99999)}"

    part_number = f"PN-{random.randint(1000, 9999)}"

    run.unit.serial_number = serial_number
    run.unit.part_number = part_number

    log.info(f"Unit info set: Serial={serial_number}, Part={part_number}")
