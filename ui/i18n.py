# SPDX-License-Identifier: GPL-3.0-or-later
"""English message IDs with lightweight, complete locale catalogs."""
import json
import os
from pathlib import Path
import tempfile

_SELECTION = 'auto'
_CATALOG = {}
_PREFERENCES = None


def system_language(environment=None):
    env = os.environ if environment is None else environment
    base = env.get('LC_ALL') or env.get('LC_MESSAGES') or env.get('LANG') or 'en'
    if base.split('.')[0] in ('C', 'POSIX'):
        return 'en'
    candidates = (env.get('LANGUAGE') or base).split(':')
    for candidate in candidates:
        language = candidate.split('.')[0].split('@')[0].replace('-', '_')
        if language in ('zh', 'zh_CN', 'zh_SG'):
            return 'zh_CN'
        if language.startswith('en'):
            return 'en'
    return 'en'


def configure(directory=None, selection=None):
    global _SELECTION, _CATALOG, _PREFERENCES
    if directory is not None:
        _PREFERENCES = Path(directory) / 'ui-preferences.json'
    if selection is None and _PREFERENCES and _PREFERENCES.exists():
        try:
            selection = json.loads(_PREFERENCES.read_text()).get('language', 'auto')
        except (ValueError, OSError, AttributeError):
            selection = 'auto'
    _SELECTION = selection if selection in ('auto', 'en', 'zh_CN') else 'auto'
    language = system_language() if _SELECTION == 'auto' else _SELECTION
    _CATALOG = json.loads((Path(__file__).parent / 'locales/zh_CN.json').read_text()) if language == 'zh_CN' else {}


def get_selection():
    return _SELECTION


def save_language(selection, directory):
    if selection not in ('auto', 'en', 'zh_CN'):
        raise ValueError('unsupported language')
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    destination = directory / 'ui-preferences.json'
    preferences = json.loads(destination.read_text()) if destination.exists() else {}
    if not isinstance(preferences, dict):
        raise ValueError('invalid preferences')
    preferences['language'] = selection
    with tempfile.NamedTemporaryFile(mode='w', dir=directory, prefix='.language-', delete=False) as file:
        temporary = Path(file.name)
        try:
            json.dump(preferences, file, ensure_ascii=False, indent=2)
            file.flush()
            os.fsync(file.fileno())
            temporary.replace(destination)
        finally:
            temporary.unlink(missing_ok=True)
    configure(directory, selection)


def _(message):
    return _CATALOG.get(message, message)


def N_(message):
    """Mark messages translated later, after the user chooses a language."""
    return message
