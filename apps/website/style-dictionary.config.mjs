export default {
  source: ["tokens/**/*.json"],
  platforms: {
    css: {
      transformGroup: "css",
      buildPath: "app/",
      files: [
        {
          destination: "generated-tokens.css",
          format: "css/variables",
          options: { outputReferences: true },
        },
      ],
    },
  },
};
