import os
import sys


def setup_each(phase, log, run, ui):
    log.info(f"Setting up slot-level resources for {run.slot_id}")
    log.info("Slot-level plugs will be initialized before this phase")
    
