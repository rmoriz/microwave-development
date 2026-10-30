/**
 * The guides live in `docs/` at the repository root, so their links point at
 * files outside that folder (`../crates/...`, `../Dockerfile`) or at sibling
 * repository files (`superpowers/specs/...`). Those only resolve on GitHub;
 * Docusaurus would treat them as routes on this site. Rewrite them to absolute
 * GitHub URLs and leave every source file untouched.
 *
 * Site-local links stay as they are: `./guide.md` (resolved by Docusaurus),
 * `/route`, `#anchor`, and anything with a scheme.
 */
const REPOSITORY = 'https://github.com/rmoriz/microwave-development/blob/main';

function walk(node, visit) {
  visit(node);
  if (Array.isArray(node.children)) {
    for (const child of node.children) {
      walk(child, visit);
    }
  }
}

function isRepositoryLink(url) {
  return !/^(?:[a-z][a-z0-9+.-]*:|\/|#|\.\/)/i.test(url);
}

function toRepositoryPath(url) {
  let path = url;
  while (path.startsWith('../')) {
    path = path.slice(3);
  }
  if (!url.startsWith('../')) {
    path = `docs/${path}`;
  }
  return path;
}

export default function githubRelativeLinks() {
  return (tree) => {
    walk(tree, (node) => {
      if (node.type !== 'link' || typeof node.url !== 'string') {
        return;
      }
      if (!isRepositoryLink(node.url)) {
        return;
      }
      const [path, ...anchorParts] = toRepositoryPath(node.url).split('#');
      const anchor = anchorParts.length > 0 ? `#${anchorParts.join('#')}` : '';
      node.url = `${REPOSITORY}/${path}${anchor}`;
    });
  };
}
