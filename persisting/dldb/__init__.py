def connect(
    db_name,
    use_memory_queue: bool = False,
    flush_every: int = 1000,
    storage_options: dict = None,
    model=None,
    **kwargs,
) -> "LanceSession":
    import atexit
    from dldb.session import LanceSession
    from dldb.instrumentation import instrument_session

    assert not use_memory_queue, "memory queue not supported, will support later"

    if model is not None and model != "" and model not in {"metrics", "debug"}:
        raise ValueError(f"Invalid model={model!r}. Allowed: None, '', 'metrics', 'debug'")

    session = LanceSession(
        db_name=db_name,
        use_memory_queue=use_memory_queue,
        flush_every=flush_every,
        storage_options=storage_options,
        model=model,
        **kwargs,
    )
    instrument_session(session)
    atexit.register(session.shutdown)
    return session
