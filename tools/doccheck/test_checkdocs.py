"""Regressions for checkdocs.py: every kind of problem must fail the check."""
import os
import subprocess
import sys
import tempfile
import unittest

CHECK = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'checkdocs.py')


def check(files):
    """Run the checker over `files` (name -> text) in a fresh folder."""
    with tempfile.TemporaryDirectory() as root:
        for name, text in files.items():
            path = os.path.join(root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, 'w', encoding='utf-8') as handle:
                handle.write(text)
        # Outside any checkout, so the walk fallback lists the files.
        environment = dict(os.environ, GIT_CEILING_DIRECTORIES=os.path.dirname(root))
        return subprocess.run([sys.executable, CHECK, root], capture_output=True, text=True, env=environment)


class CheckDocs(unittest.TestCase):
    def test_clean_documents_pass(self):
        result = check({
            'a.md': '# Title\n\n[b](docs/b.md#part-two)\n\n| x | y |\n|---|---|\n| `a|b` | c \\| d |\n| ``a|`b`` | e |\n',
            'docs/b.md': '# B\n\n## Part two\n\n```\n[not a link](missing.md)\n```\n',
        })
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn('0 problems across 2 files', result.stdout)

    def test_a_missing_link_target_fails(self):
        result = check({'a.md': '[b](missing.md)\n'})
        self.assertEqual(result.returncode, 1)
        self.assertIn('BROKEN-FILE a.md:1: missing.md', result.stdout)

    def test_a_missing_anchor_fails(self):
        result = check({'a.md': '[b](b.md#absent)\n', 'b.md': '# Present\n'})
        self.assertEqual(result.returncode, 1)
        self.assertIn('BROKEN-ANCHOR a.md:1: b.md#absent', result.stdout)

    def test_an_unclosed_fence_fails(self):
        result = check({'a.md': '# Title\n\n```\nunclosed\n'})
        self.assertEqual(result.returncode, 1)
        self.assertIn('FENCE odd count in a.md', result.stdout)

    def test_a_scratch_path_fails(self):
        result = check({'a.md': 'See /tmp/notes for details.\n'})
        self.assertEqual(result.returncode, 1)
        self.assertIn('LEAK a.md:1', result.stdout)

    def test_a_row_with_the_wrong_cell_count_fails(self):
        result = check({'a.md': '| x | y |\n|---|---|\n| 1 |\n| 1 | 2 | 3 |\n\n| p |\n|---|\n| q |\n'})
        self.assertEqual(result.returncode, 1)
        self.assertIn('TABLE a.md:3: 1 cells in a table of 2 columns', result.stdout)
        self.assertIn('TABLE a.md:4: 3 cells in a table of 2 columns', result.stdout)
        self.assertIn('2 problems', result.stdout)

    def test_ignored_files_in_a_checkout_are_skipped(self):
        with tempfile.TemporaryDirectory() as root:
            subprocess.run(['git', 'init', '-q', root], check=True)
            for name, text in {'.gitignore': 'dist/\n', 'a.md': '# Fine\n', 'dist/b.md': '[x](missing.md)\n'}.items():
                path = os.path.join(root, name)
                os.makedirs(os.path.dirname(path), exist_ok=True)
                with open(path, 'w', encoding='utf-8') as handle:
                    handle.write(text)
            result = subprocess.run([sys.executable, CHECK, root], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn('0 problems across 1 files', result.stdout)


if __name__ == '__main__':
    unittest.main()
