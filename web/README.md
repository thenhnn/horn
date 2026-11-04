# Website

This directory contains the website source: Jinja templates, stylesheets,
logo assets, and a small Python renderer. `default.nix` builds the complete
static site using reference data from the Horn IDL compiler.

From the repository root, run:

```sh
just site
python3 -m http.server --directory work/site-result
```

`work/site-result` is a symlink to the immutable Nix build output. To choose a
different link, run `nix-build web --out-link PATH`. The build uses the pinned
Nix dependencies and needs no Python packages installed locally. Only build
documentation from trusted IDL schemas: schema modules are executable Python.

## GitHub Pages

`.github/workflows/pages.yml` builds and deploys the site on pushes to `slop`.
It also supports manual runs when the workflow is present on the default branch.
The workflow builds the site with Nix and uploads the result as a Pages artifact.

To set up `horn.nhnn.net`:

1. In the repository's **Settings → Pages**, select **GitHub Actions** as the
   publishing source and set the custom domain to `horn.nhnn.net`.
2. At the DNS provider, point the `horn` CNAME record to `thenhnn.github.io`.
3. After DNS and certificate provisioning finish, enable **Enforce HTTPS** in
   Pages settings. If the `github-pages` environment restricts deployment
   branches, allow `slop`.

The Pages custom-domain setting is managed by GitHub; a `CNAME` file is not
required when deploying with a custom Actions workflow.
