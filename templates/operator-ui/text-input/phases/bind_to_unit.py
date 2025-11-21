def bind_to_unit(run):
    log.info("Text input bound to unit.serial_number")
    log.info("The value entered will be automatically saved to unit metadata")
    log.info("This binding happens after the operator submits the form")
    log.info("")
    log.info(f"Current unit serial number: {run.unit.serial_number if hasattr(run.unit, 'serial_number') else 'Not set yet'}")
