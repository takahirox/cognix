"""Keep SIGINT from interrupting side-effect ownership and evidence updates."""

import signal
import threading
from contextlib import contextmanager


@contextmanager
def defer_sigint(cancel, until=None):
    # A caller can keep the guard alive until its execution ledger is updated,
    # beyond the tool's return and resource cleanup.
    if until is not None:
        yield until.enter_context(defer_sigint(cancel))
        return
    # Python delivers signals only on the main thread. Worker callers already
    # use the shared event and must not change the process-wide signal handler.
    if threading.current_thread() is not threading.main_thread():
        yield cancel.is_set
        return
    requested = False

    def request(*_):
        nonlocal requested
        # Event.set() takes a lock, which a signal could have interrupted.
        requested = True

    def canceled():
        if requested:
            cancel.set()
        return cancel.is_set()

    previous = signal.getsignal(signal.SIGINT)
    signal.signal(signal.SIGINT, request)
    try:
        yield canceled
    finally:
        signal.signal(signal.SIGINT, previous)
        if requested:
            cancel.set()
