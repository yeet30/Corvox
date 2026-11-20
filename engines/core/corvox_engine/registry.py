"""Maps engine ids (the ones in engines/catalog.json) to adapter classes. Imports are lazy so
each environment only needs the libraries for the engines it actually runs."""

from __future__ import annotations


def make_adapter(model: str):
    if model == "pocket":
        from .adapters.pocket import PocketAdapter

        return PocketAdapter()
    if model == "pocket-large":
        from .adapters.pocket import PocketLargeAdapter

        return PocketLargeAdapter()
    if model == "pocket-tr":
        from .adapters.pocket import PocketTurkishAdapter

        return PocketTurkishAdapter()
    if model == "xtts":
        from .adapters.xtts import XttsAdapter

        return XttsAdapter()
    if model == "chatterbox":
        from .adapters.chatterbox import ChatterboxAdapter

        return ChatterboxAdapter()
    raise ValueError(f"unknown engine {model!r}")
