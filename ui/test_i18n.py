# SPDX-License-Identifier: GPL-3.0-or-later
import ast
import json
from pathlib import Path
import string
import tempfile
import unittest
import i18n

ROOT = Path(__file__).parent


class TranslationTests(unittest.TestCase):
    def test_system_language_defaults_to_english_and_recognizes_chinese(self):
        for env in ({'LANG': 'en_US.UTF-8'}, {'LANG': 'en_GB.UTF-8'}, {'LANG': 'C.UTF-8'}, {'LANG': 'fr_FR.UTF-8'}):
            self.assertEqual(i18n.system_language(env), 'en')
        self.assertEqual(i18n.system_language({'LANG': 'zh_CN.UTF-8'}), 'zh_CN')
        self.assertEqual(i18n.system_language({'LANG': 'en_US.UTF-8', 'LC_MESSAGES': 'zh_CN.UTF-8'}), 'zh_CN')
        self.assertEqual(i18n.system_language({'LANG': 'zh_CN.UTF-8', 'LANGUAGE': 'en:zh_CN'}), 'en')

    def test_catalog_covers_every_marked_message_and_preserves_placeholders(self):
        catalog = json.loads((ROOT / 'locales/zh_CN.json').read_text())
        messages = set()
        for filename in ['app.py', 'model.py', 'canvas.py', 'tray.py']:
            tree = ast.parse((ROOT / filename).read_text())
            for node in ast.walk(tree):
                if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id in ('_', 'N_') and node.args and isinstance(node.args[0], ast.Constant):
                    messages.add(node.args[0].value)
        self.assertFalse(messages - catalog.keys(), f'Missing translations: {messages - catalog.keys()}')
        self.assertGreater(len(messages), 200)
        formatter = string.Formatter()
        for message in messages:
            source = {name for _, name, _, _ in formatter.parse(message) if name}
            translated = {name for _, name, _, _ in formatter.parse(catalog[message]) if name}
            self.assertEqual(source, translated, message)
            self.assertTrue(catalog[message].strip())

    def test_language_preference_is_private_and_preserves_unrelated_values(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'ui-preferences.json'
            path.write_text(json.dumps({'another_preference': 3}))
            i18n.save_language('zh_CN', directory)
            self.assertEqual(i18n._('Overview'), '总览')
            self.assertEqual(json.loads(path.read_text())['another_preference'], 3)
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            i18n.save_language('en', directory)
            self.assertEqual(i18n._('Overview'), 'Overview')
            path.write_text('unfinished user edit')
            with self.assertRaises(ValueError):
                i18n.save_language('zh_CN', directory)
            self.assertEqual(path.read_text(), 'unfinished user edit')
        i18n.configure(selection='en')


if __name__ == '__main__':
    unittest.main()
