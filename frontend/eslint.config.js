import reactConfig from "@anarkisti/eslint-config/react";
import pluginRouter from "@tanstack/eslint-plugin-router";

export default [...pluginRouter.configs["flat/recommended"], ...reactConfig];
