/** @type {import('@docusaurus/plugin-content-docs').SidebarsConfig} */
export default {
  sidebar: [
    {
      type: 'doc',
      id: 'index',
    },
    {
      type: 'category',
      label: 'Getting Started',
      link: {
        type: 'doc',
        id: 'getting_started',
      },
      items: [
        {
          type: 'autogenerated',
          dirName: 'getting_started',
        },
      ],
    },
    {
      type: 'category',
      label: 'The Noir Language',
      items: [
        {
          type: 'autogenerated',
          dirName: 'noir',
        },
      ],
    },
    {
      type: 'html',
      value: '<div class="divider"></div>',
      defaultStyle: true,
    },
    {
      type: 'category',
      label: 'How To Guides',
      items: [
        {
          type: 'autogenerated',
          dirName: 'how_to',
        },
      ],
    },
    {
      type: 'category',
      label: 'Explainers',
      items: [
        {
          type: 'autogenerated',
          dirName: 'explainers',
        },
      ],
    },
    {
      type: 'category',
      label: 'Tutorials',
      items: [
        {
          type: 'autogenerated',
          dirName: 'tutorials',
        },
      ],
    },
    {
      type: 'category',
      label: 'Reference',
      items: [{ type: 'autogenerated', dirName: 'reference' }],
    },
    {
      type: 'category',
      label: 'Tooling',
      items: [{ type: 'autogenerated', dirName: 'tooling' }],
    },
    {
      type: 'html',
      value: '<div class="divider"></div>',
      defaultStyle: true,
    },
    {
      type: 'doc',
      id: 'migration_notes',
      label: 'Migration notes',
    },
  ],
};
