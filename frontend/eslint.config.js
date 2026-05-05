import pluginRouter from "@tanstack/eslint-plugin-router";
import reactConfig from "eslint-config/react";

export default [...pluginRouter.configs["flat/recommended"], ...reactConfig];
